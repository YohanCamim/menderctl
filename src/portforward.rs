use std::{collections::HashMap, net::SocketAddr, sync::Arc};

use anyhow::{Context, Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use rmpv::Value;
use serde::Deserialize;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
    sync::{Mutex, mpsc, watch},
};
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

use crate::{
    api::MenderApi,
    protocol::{
        Accept, Open, PROTO_CONTROL, PROTO_PORT_FORWARD, PROTO_PORT_FORWARD_V2, PortForwardOpen,
        ProtoMessage, ProtocolError, connect_device, send_proto,
    },
    ui,
};

const CONNECTION_ID: &str = "connection_id";

#[derive(Debug, Deserialize)]
struct PortForwardError {
    #[serde(rename = "err")]
    error: String,
    #[serde(rename = "msgtype", default)]
    message_type: String,
}

impl PortForwardError {
    fn is_stale_connection(&self) -> bool {
        self.error == "unknown connection"
            && matches!(self.message_type.as_str(), "ack" | "forward" | "stop")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PortMapping {
    pub protocol: String,
    pub local_port: u16,
    pub remote_host: String,
    pub remote_port: u16,
}

impl PortMapping {
    pub fn parse(value: &str) -> Result<Self> {
        let (protocol, value) = value.split_once('/').unwrap_or(("tcp", value));
        if !matches!(protocol, "tcp" | "udp") {
            bail!("unknown protocol `{protocol}`; use tcp or udp");
        }
        let parts: Vec<_> = value.split(':').collect();
        let (local, remote_host, remote) = match parts.as_slice() {
            [port] => (*port, "127.0.0.1", *port),
            [local, remote] => (*local, "127.0.0.1", *remote),
            [local, host, remote] if !host.is_empty() => (*local, *host, *remote),
            _ => bail!("invalid mapping `{value}`"),
        };
        let local_port = parse_port(local)?;
        let remote_port = parse_port(remote)?;
        Ok(Self {
            protocol: protocol.to_owned(),
            local_port,
            remote_host: remote_host.to_owned(),
            remote_port,
        })
    }
}

pub async fn run(
    api: &MenderApi,
    server: &str,
    token: &str,
    skip_verify: bool,
    device_id: &str,
    bind_host: &str,
    mapping_values: &[String],
) -> Result<()> {
    if !api.device_connected(device_id).await? {
        bail!("device {device_id} is not connected");
    }
    let device_name = api.display_name(device_id).await;
    ui::device_banner(
        &device_name,
        device_id,
        "Port forwarding — press CTRL+C to stop",
    );
    let mappings = mapping_values
        .iter()
        .map(|value| PortMapping::parse(value))
        .collect::<Result<Vec<_>>>()?;
    let socket = connect_device(server, device_id, token, skip_verify).await?;
    let (mut sink, mut stream) = socket.split();
    let open = ProtoMessage::new(PROTO_CONTROL, "open")
        .with_body(rmp_serde::to_vec_named(&Open { versions: vec![1] })?);
    send_proto(&mut sink, open).await?;

    let (protocol, session_id) = loop {
        let Some(message) = stream.next().await else {
            bail!("connection closed during port-forward negotiation");
        };
        match message? {
            Message::Binary(bytes) => {
                let message = ProtoMessage::decode(&bytes)?;
                match message.header.message_type.as_str() {
                    "accept" => {
                        let accept: Accept = rmp_serde::from_slice(&message.body)?;
                        let protocol = if accept.protocols.contains(&PROTO_PORT_FORWARD_V2) {
                            PROTO_PORT_FORWARD_V2
                        } else if accept.protocols.contains(&PROTO_PORT_FORWARD) {
                            PROTO_PORT_FORWARD
                        } else {
                            bail!("port forwarding is not enabled on this device");
                        };
                        break (protocol, message.header.session_id);
                    }
                    "error" => bail!("negotiation rejected: {}", protocol_error(&message.body)),
                    _ => bail!("unexpected negotiation response"),
                }
            }
            Message::Ping(payload) => sink.send(Message::Pong(payload)).await?,
            Message::Close(_) => bail!("connection closed during negotiation"),
            _ => {}
        }
    };

    let routes: Routes = Arc::new(Mutex::new(HashMap::new()));
    let (out_tx, mut out_rx) = mpsc::channel::<ProtoMessage>(128);
    let (cancel_tx, cancel_rx) = watch::channel(false);
    for mapping in mappings {
        match mapping.protocol.as_str() {
            "tcp" => {
                let listener = TcpListener::bind((bind_host, mapping.local_port))
                    .await
                    .with_context(|| {
                        format!("failed to listen on {bind_host}:{}", mapping.local_port)
                    })?;
                println!(
                    "Forwarding tcp/{bind_host}:{} -> tcp/{}:{}",
                    mapping.local_port, mapping.remote_host, mapping.remote_port
                );
                tokio::spawn(accept_connections(
                    listener,
                    mapping,
                    protocol,
                    session_id.clone(),
                    out_tx.clone(),
                    routes.clone(),
                    cancel_rx.clone(),
                ));
            }
            "udp" => {
                let socket = UdpSocket::bind((bind_host, mapping.local_port))
                    .await
                    .with_context(|| {
                        format!(
                            "failed to listen for UDP on {bind_host}:{}",
                            mapping.local_port
                        )
                    })?;
                println!(
                    "Forwarding udp/{bind_host}:{} -> udp/{}:{}",
                    mapping.local_port, mapping.remote_host, mapping.remote_port
                );
                tokio::spawn(handle_udp(
                    socket,
                    mapping,
                    protocol,
                    session_id.clone(),
                    out_tx.clone(),
                    routes.clone(),
                    cancel_rx.clone(),
                ));
            }
            _ => unreachable!(),
        }
    }

    let mut close_session = true;
    let outcome = loop {
        tokio::select! {
            Some(outgoing) = out_rx.recv() => {
                if let Err(error) = send_proto(&mut sink, outgoing).await {
                    close_session = false;
                    break Err(error);
                }
            }
            incoming = stream.next() => {
                let Some(incoming) = incoming else {
                    close_session = false;
                    break Err(anyhow!("the server closed the port-forward connection"));
                };
                match incoming? {
                    Message::Binary(bytes) => {
                        let message = ProtoMessage::decode(&bytes)?;
                        if message.header.protocol == PROTO_CONTROL && message.header.message_type == "ping" {
                            send_proto(
                                &mut sink,
                                ProtoMessage::new(PROTO_CONTROL, "pong").with_session(&session_id),
                            ).await?;
                        } else if message.header.protocol == PROTO_CONTROL
                            && message.header.message_type == "close"
                        {
                            close_session = false;
                            break Err(anyhow!("the device closed the port-forward session"));
                        } else if message.header.protocol == protocol && message.header.message_type == "error" {
                            let error = port_forward_error(&message.body);
                            if error.is_stale_connection() {
                                tracing::debug!(
                                    message_type = %error.message_type,
                                    "ignoring a late port-forward message for a closed connection"
                                );
                                continue;
                            }
                            let message_type = if error.message_type.is_empty() {
                                String::new()
                            } else {
                                format!(" while handling `{}`", error.message_type)
                            };
                            break Err(anyhow!(
                                "port-forward error{message_type}: {}",
                                error.error
                            ));
                        } else if message.header.protocol == protocol
                            && let Some(connection_id) = message.string_property(CONNECTION_ID)
                        {
                            let route = routes.lock().await.get(&connection_id).cloned();
                            if let Some(route) = route {
                                let _ = route.send(message).await;
                            }
                        }
                    }
                    Message::Ping(payload) => sink.send(Message::Pong(payload)).await?,
                    Message::Close(_) => {
                        close_session = false;
                        break Err(anyhow!("the server closed the connection"));
                    }
                    _ => {}
                }
            }
            result = tokio::signal::ctrl_c() => {
                result?;
                break Ok(());
            }
        }
    };

    let _ = cancel_tx.send(true);
    if close_session {
        let _ = send_proto(&mut sink, ProtoMessage::new(PROTO_CONTROL, "close")).await;
        let _ = sink.close().await;
    }
    outcome
}

type Routes = Arc<Mutex<HashMap<String, mpsc::Sender<ProtoMessage>>>>;

async fn accept_connections(
    listener: TcpListener,
    mapping: PortMapping,
    protocol: u16,
    session_id: String,
    outgoing: mpsc::Sender<ProtoMessage>,
    routes: Routes,
    mut cancel: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((stream, address)) = accepted else { break; };
                println!("Local connection from {address} on port {}", mapping.local_port);
                tokio::spawn(handle_connection(
                    stream,
                    mapping.clone(),
                    protocol,
                    session_id.clone(),
                    outgoing.clone(),
                    routes.clone(),
                    cancel.clone(),
                ));
            }
            changed = cancel.changed() => {
                if changed.is_err() || *cancel.borrow() { break; }
            }
        }
    }
}

async fn handle_connection(
    stream: TcpStream,
    mapping: PortMapping,
    protocol: u16,
    session_id: String,
    outgoing: mpsc::Sender<ProtoMessage>,
    routes: Routes,
    mut cancel: watch::Receiver<bool>,
) {
    let connection_id = Uuid::new_v4().to_string();
    let (route_tx, mut route_rx) = mpsc::channel(32);
    routes.lock().await.insert(connection_id.clone(), route_tx);
    let body = match port_forward_open_body(&mapping, "tcp") {
        Ok(body) => body,
        Err(_) => return,
    };
    if outgoing
        .send(
            ProtoMessage::new(protocol, "new")
                .with_session(&session_id)
                .with_property(CONNECTION_ID, Value::from(connection_id.clone()))
                .with_body(body),
        )
        .await
        .is_err()
    {
        return;
    }

    let (mut reader, mut writer) = stream.into_split();
    let mut buffer = vec![0_u8; 4096];
    let mut started = false;
    let mut ready_to_send = true;
    let notify_remote = loop {
        tokio::select! {
            message = route_rx.recv() => {
                let Some(message) = message else { break false; };
                match message.header.message_type.as_str() {
                    "new" => started = true,
                    "forward" => {
                        if writer.write_all(&message.body).await.is_err() { break true; }
                        if protocol == PROTO_PORT_FORWARD {
                            let ack = ProtoMessage::new(protocol, "ack")
                                .with_session(&session_id)
                                .with_property(CONNECTION_ID, Value::from(connection_id.clone()));
                            if outgoing.send(ack).await.is_err() { break false; }
                        }
                    }
                    "ack" if protocol == PROTO_PORT_FORWARD => ready_to_send = true,
                    "stop" => break false,
                    _ => {}
                }
            }
            read = reader.read(&mut buffer), if started && ready_to_send => {
                let Ok(read) = read else { break true; };
                if read == 0 { break true; }
                if protocol == PROTO_PORT_FORWARD { ready_to_send = false; }
                let message = ProtoMessage::new(protocol, "forward")
                    .with_session(&session_id)
                    .with_property(CONNECTION_ID, Value::from(connection_id.clone()))
                    .with_body(buffer[..read].to_vec());
                if outgoing.send(message).await.is_err() { break false; }
            }
            changed = cancel.changed() => {
                if changed.is_err() || *cancel.borrow() { break false; }
            }
        }
    };
    routes.lock().await.remove(&connection_id);
    if let Some(stop) = stop_message(protocol, session_id, connection_id, notify_remote) {
        let _ = outgoing.send(stop).await;
    }
}

async fn handle_udp(
    socket: UdpSocket,
    mapping: PortMapping,
    protocol: u16,
    session_id: String,
    outgoing: mpsc::Sender<ProtoMessage>,
    routes: Routes,
    mut cancel: watch::Receiver<bool>,
) {
    let connection_id = Uuid::new_v4().to_string();
    let (route_tx, mut route_rx) = mpsc::channel(32);
    routes.lock().await.insert(connection_id.clone(), route_tx);
    let body = match port_forward_open_body(&mapping, "udp") {
        Ok(body) => body,
        Err(_) => return,
    };
    if outgoing
        .send(
            ProtoMessage::new(protocol, "new")
                .with_session(&session_id)
                .with_property(CONNECTION_ID, Value::from(connection_id.clone()))
                .with_body(body),
        )
        .await
        .is_err()
    {
        return;
    }

    let mut buffer = vec![0_u8; 65_535];
    let mut source_address: Option<SocketAddr> = None;
    let mut started = false;
    let mut ready_to_send = true;
    let notify_remote = loop {
        tokio::select! {
            message = route_rx.recv() => {
                let Some(message) = message else { break false; };
                match message.header.message_type.as_str() {
                    "new" => started = true,
                    "forward" => {
                        if let Some(address) = source_address
                            && socket.send_to(&message.body, address).await.is_err()
                        {
                            break true;
                        }
                        if protocol == PROTO_PORT_FORWARD {
                            let ack = ProtoMessage::new(protocol, "ack")
                                .with_session(&session_id)
                                .with_property(CONNECTION_ID, Value::from(connection_id.clone()));
                            if outgoing.send(ack).await.is_err() { break false; }
                        }
                    }
                    "ack" if protocol == PROTO_PORT_FORWARD => ready_to_send = true,
                    "stop" => break false,
                    _ => {}
                }
            }
            received = socket.recv_from(&mut buffer), if started && ready_to_send => {
                let Ok((read, address)) = received else { break true; };
                source_address = Some(address);
                if protocol == PROTO_PORT_FORWARD { ready_to_send = false; }
                let message = ProtoMessage::new(protocol, "forward")
                    .with_session(&session_id)
                    .with_property(CONNECTION_ID, Value::from(connection_id.clone()))
                    .with_body(buffer[..read].to_vec());
                if outgoing.send(message).await.is_err() { break false; }
            }
            changed = cancel.changed() => {
                if changed.is_err() || *cancel.borrow() { break false; }
            }
        }
    };
    routes.lock().await.remove(&connection_id);
    if let Some(stop) = stop_message(protocol, session_id, connection_id, notify_remote) {
        let _ = outgoing.send(stop).await;
    }
}

fn port_forward_open_body(mapping: &PortMapping, protocol: &str) -> Result<Vec<u8>> {
    Ok(rmp_serde::to_vec_named(&PortForwardOpen {
        remote_host: &mapping.remote_host,
        remote_port: mapping.remote_port,
        protocol,
    })?)
}

fn stop_message(
    protocol: u16,
    session_id: String,
    connection_id: String,
    notify_remote: bool,
) -> Option<ProtoMessage> {
    notify_remote.then(|| {
        ProtoMessage::new(protocol, "stop")
            .with_session(session_id)
            .with_property(CONNECTION_ID, Value::from(connection_id))
    })
}

fn protocol_error(body: &[u8]) -> String {
    rmp_serde::from_slice::<ProtocolError>(body)
        .map(|error| error.error)
        .unwrap_or_else(|_| String::from_utf8_lossy(body).into_owned())
}

fn port_forward_error(body: &[u8]) -> PortForwardError {
    rmp_serde::from_slice(body).unwrap_or_else(|_| PortForwardError {
        error: String::from_utf8_lossy(body).into_owned(),
        message_type: String::new(),
    })
}

fn parse_port(value: &str) -> Result<u16> {
    let port: u16 = value
        .parse()
        .with_context(|| format!("invalid port `{value}`"))?;
    if port == 0 {
        bail!("the port must be between 1 and 65535");
    }
    Ok(port)
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use serde::{Deserialize, Serialize};
    use tokio::{
        net::{TcpListener, TcpStream},
        sync::{Mutex, mpsc, watch},
    };

    use super::{
        CONNECTION_ID, PortForwardError, PortMapping, Routes, handle_connection,
        port_forward_error, port_forward_open_body, stop_message,
    };
    use crate::protocol::{PROTO_PORT_FORWARD_V2, ProtoMessage};

    #[derive(Debug, Deserialize, Eq, PartialEq)]
    struct PortForwardOpen {
        remote_host: String,
        remote_port: u16,
        protocol: String,
    }

    #[derive(Serialize)]
    struct EncodedPortForwardError<'a> {
        #[serde(rename = "err")]
        error: &'a str,
        #[serde(rename = "msgtype")]
        message_type: &'a str,
    }

    #[test]
    fn parses_supported_mapping_forms() {
        assert_eq!(
            PortMapping::parse("8000").unwrap(),
            PortMapping {
                protocol: "tcp".into(),
                local_port: 8000,
                remote_host: "127.0.0.1".into(),
                remote_port: 8000
            }
        );
        assert_eq!(
            PortMapping::parse("tcp/8000:192.168.1.10:80").unwrap(),
            PortMapping {
                protocol: "tcp".into(),
                local_port: 8000,
                remote_host: "192.168.1.10".into(),
                remote_port: 80
            }
        );
    }

    #[test]
    fn rejects_invalid_ports() {
        assert!(PortMapping::parse("0:22").is_err());
        assert!(PortMapping::parse("70000:22").is_err());
    }

    #[test]
    fn explicit_remote_host_is_encoded_in_open_request() {
        let mapping = PortMapping::parse("8080:192.168.1.123:4443").unwrap();
        let body = port_forward_open_body(&mapping, "tcp").unwrap();
        let request: PortForwardOpen = rmp_serde::from_slice(&body).unwrap();

        assert_eq!(
            request,
            PortForwardOpen {
                remote_host: "192.168.1.123".into(),
                remote_port: 4443,
                protocol: "tcp".into(),
            }
        );
    }

    #[test]
    fn remote_stop_is_not_echoed_back() {
        assert!(stop_message(3, "session".into(), "connection".into(), false).is_none());

        let stop = stop_message(3, "session".into(), "connection".into(), true).unwrap();
        assert_eq!(stop.header.message_type, "stop");
        assert_eq!(
            stop.string_property(CONNECTION_ID).as_deref(),
            Some("connection")
        );
    }

    #[tokio::test]
    async fn remote_tcp_stop_closes_locally_without_echoing_stop() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let (client, accepted) = tokio::join!(TcpStream::connect(address), listener.accept());
        let _client = client.unwrap();
        let (stream, _) = accepted.unwrap();
        let mapping = PortMapping::parse("8080:192.168.1.123:4443").unwrap();
        let (outgoing, mut messages) = mpsc::channel(4);
        let routes: Routes = Arc::new(Mutex::new(HashMap::new()));
        let (_cancel, cancellation) = watch::channel(false);

        let task = tokio::spawn(handle_connection(
            stream,
            mapping,
            PROTO_PORT_FORWARD_V2,
            "session".into(),
            outgoing,
            routes.clone(),
            cancellation,
        ));

        let open = messages.recv().await.unwrap();
        let connection_id = open.string_property(CONNECTION_ID).unwrap();
        let route = routes.lock().await.get(&connection_id).cloned().unwrap();
        route
            .send(ProtoMessage::new(PROTO_PORT_FORWARD_V2, "new"))
            .await
            .unwrap();
        route
            .send(ProtoMessage::new(PROTO_PORT_FORWARD_V2, "stop"))
            .await
            .unwrap();

        task.await.unwrap();
        assert!(messages.recv().await.is_none());
    }

    #[test]
    fn ignores_only_stale_connection_errors() {
        let encode = |error, message_type| {
            rmp_serde::to_vec_named(&EncodedPortForwardError {
                error,
                message_type,
            })
            .unwrap()
        };

        for message_type in ["ack", "forward", "stop"] {
            assert!(
                port_forward_error(&encode("unknown connection", message_type))
                    .is_stale_connection()
            );
        }
        assert!(!port_forward_error(&encode("unknown connection", "new")).is_stale_connection());
        assert!(!port_forward_error(&encode("connection refused", "new")).is_stale_connection());

        let malformed = port_forward_error(b"plain-text failure");
        assert_eq!(malformed.error, "plain-text failure");
        assert!(malformed.message_type.is_empty());
    }

    #[test]
    fn stale_error_requires_a_message_type() {
        let error = PortForwardError {
            error: "unknown connection".into(),
            message_type: String::new(),
        };
        assert!(!error.is_stale_connection());
    }
}
