use std::time::Duration;

use anyhow::{Context, Result, bail};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, size};
use futures_util::{SinkExt, StreamExt};
use rmpv::Value;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    time,
};
use tokio_tungstenite::tungstenite::Message;

use crate::{
    api::MenderApi,
    protocol::{
        PROTO_CONTROL, PROTO_SHELL, ProtoMessage, ProtocolError, connect_device, send_proto,
    },
    ui,
};

const DEFAULT_WIDTH: u16 = 80;
const DEFAULT_HEIGHT: u16 = 40;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionEnd {
    Local,
    Remote,
}

pub async fn run(
    api: &MenderApi,
    server: &str,
    token: &str,
    skip_verify: bool,
    device_id: &str,
) -> Result<()> {
    if !api.device_connected(device_id).await? {
        bail!("device {device_id} is not connected");
    }
    let device_name = api.display_name(device_id).await;
    ui::device_banner(
        &device_name,
        device_id,
        "Remote terminal — press CTRL+] to leave",
    );
    eprintln!("Connecting…");
    let socket = connect_device(server, device_id, token, skip_verify)
        .await
        .context("failed to open the terminal WebSocket")?;
    let (mut sink, mut stream) = socket.split();
    let (width, height) = size().unwrap_or((DEFAULT_WIDTH, DEFAULT_HEIGHT));
    let start = ProtoMessage::new(PROTO_SHELL, "new")
        .with_property("terminal_width", Value::from(width))
        .with_property("terminal_height", Value::from(height));
    send_proto(&mut sink, start).await?;

    let session_id = loop {
        let Some(message) = stream.next().await else {
            bail!("the server closed the connection before creating the terminal");
        };
        match message? {
            Message::Binary(bytes) => {
                let message = ProtoMessage::decode(&bytes)?;
                if message.header.protocol == PROTO_SHELL && message.header.message_type == "new" {
                    if message
                        .header
                        .properties
                        .get("status")
                        .and_then(Value::as_i64)
                        == Some(2)
                    {
                        bail!(
                            "the remote terminal rejected the session: {}",
                            String::from_utf8_lossy(&message.body)
                        );
                    }
                    break message.header.session_id;
                }
            }
            Message::Ping(payload) => sink.send(Message::Pong(payload)).await?,
            Message::Close(_) => bail!("the server closed the connection"),
            _ => {}
        }
    };

    let _raw = RawMode::enable()?;
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut input = [0_u8; 4096];
    let mut dimensions = (width, height);
    let mut resize_tick = time::interval(Duration::from_millis(400));

    let session_end = loop {
        tokio::select! {
            read = stdin.read(&mut input) => {
                let read = read?;
                if read == 0 || input[..read].contains(&29) {
                    break SessionEnd::Local;
                }
                let command = ProtoMessage::new(PROTO_SHELL, "shell")
                    .with_session(&session_id)
                    .with_body(input[..read].to_vec());
                send_proto(&mut sink, command).await?;
            }
            message = stream.next() => {
                let Some(message) = message else { break SessionEnd::Remote; };
                match message? {
                    Message::Binary(bytes) => {
                        let message = ProtoMessage::decode(&bytes)?;
                        match message.header.protocol {
                            PROTO_CONTROL => match message.header.message_type.as_str() {
                                "ping" => {
                                    send_proto(
                                        &mut sink,
                                        ProtoMessage::new(PROTO_CONTROL, "pong")
                                            .with_session(&session_id),
                                    ).await?;
                                }
                                "close" => break SessionEnd::Remote,
                                "error" => {
                                    bail!(
                                        "the remote terminal closed the session: {}",
                                        protocol_error(&message.body)
                                    );
                                }
                                _ => {}
                            },
                            PROTO_SHELL => match message.header.message_type.as_str() {
                                "shell" => {
                                    stdout.write_all(&message.body).await?;
                                    stdout.flush().await?;
                                }
                                "ping" => {
                                    send_proto(
                                        &mut sink,
                                        ProtoMessage::new(PROTO_SHELL, "pong").with_session(&session_id),
                                    ).await?;
                                }
                                "stop" => break SessionEnd::Remote,
                                "error" => {
                                    bail!(
                                        "remote terminal error: {}",
                                        protocol_error(&message.body)
                                    );
                                }
                                _ => {}
                            },
                            _ => {}
                        }
                    }
                    Message::Ping(payload) => sink.send(Message::Pong(payload)).await?,
                    Message::Close(_) => break SessionEnd::Remote,
                    _ => {}
                }
            }
            _ = resize_tick.tick() => {
                if let Ok(new_dimensions) = size()
                    && new_dimensions != dimensions
                {
                    dimensions = new_dimensions;
                    let resize = ProtoMessage::new(PROTO_SHELL, "resize")
                        .with_session(&session_id)
                        .with_property("terminal_width", Value::from(dimensions.0))
                        .with_property("terminal_height", Value::from(dimensions.1));
                    send_proto(&mut sink, resize).await?;
                }
            }
        }
    };

    if let Some(stop) = stop_message(session_end, session_id) {
        send_proto(&mut sink, stop).await?;
        sink.close().await?;
    }
    Ok(())
}

fn stop_message(session_end: SessionEnd, session_id: String) -> Option<ProtoMessage> {
    (session_end == SessionEnd::Local)
        .then(|| ProtoMessage::new(PROTO_SHELL, "stop").with_session(session_id))
}

fn protocol_error(body: &[u8]) -> String {
    rmp_serde::from_slice::<ProtocolError>(body)
        .map(|error| error.error)
        .unwrap_or_else(|_| String::from_utf8_lossy(body).into_owned())
}

struct RawMode;

impl RawMode {
    fn enable() -> Result<Self> {
        enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

#[cfg(test)]
mod tests {
    use super::{SessionEnd, stop_message};

    #[test]
    fn remote_session_end_does_not_require_a_stop_message() {
        assert!(stop_message(SessionEnd::Remote, "session".into()).is_none());

        let stop = stop_message(SessionEnd::Local, "session".into()).unwrap();
        assert_eq!(stop.header.message_type, "stop");
        assert_eq!(stop.header.session_id, "session");
    }
}
