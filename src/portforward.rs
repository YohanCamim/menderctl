use std::{collections::HashMap, net::SocketAddr, sync::Arc};

use anyhow::{Context, Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use rmpv::Value;
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
};

const CONNECTION_ID: &str = "connection_id";

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

    let outcome = loop {
        tokio::select! {
            Some(outgoing) = out_rx.recv() => {
                if let Err(error) = send_proto(&mut sink, outgoing).await {
                    break Err(error);
                }
            }
            incoming = stream.next() => {
                let Some(incoming) = incoming else {
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
                        } else if message.header.protocol == protocol && message.header.message_type == "error" {
                            break Err(anyhow!("port-forward error: {}", protocol_error(&message.body)));
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
                    Message::Close(_) => break Err(anyhow!("the server closed the connection")),
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
    let _ = send_proto(&mut sink, ProtoMessage::new(PROTO_CONTROL, "close")).await;
    let _ = sink.close().await;
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
    let body = match rmp_serde::to_vec_named(&PortForwardOpen {
        remote_host: &mapping.remote_host,
        remote_port: mapping.remote_port,
        protocol: "tcp",
    }) {
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
    loop {
        tokio::select! {
            message = route_rx.recv() => {
                let Some(message) = message else { break; };
                match message.header.message_type.as_str() {
                    "new" => started = true,
                    "forward" => {
                        if writer.write_all(&message.body).await.is_err() { break; }
                        if protocol == PROTO_PORT_FORWARD {
                            let ack = ProtoMessage::new(protocol, "ack")
                                .with_session(&session_id)
                                .with_property(CONNECTION_ID, Value::from(connection_id.clone()));
                            if outgoing.send(ack).await.is_err() { break; }
                        }
                    }
                    "ack" if protocol == PROTO_PORT_FORWARD => ready_to_send = true,
                    "stop" => break,
                    _ => {}
                }
            }
            read = reader.read(&mut buffer), if started && ready_to_send => {
                let Ok(read) = read else { break; };
                if read == 0 { break; }
                if protocol == PROTO_PORT_FORWARD { ready_to_send = false; }
                let message = ProtoMessage::new(protocol, "forward")
                    .with_session(&session_id)
                    .with_property(CONNECTION_ID, Value::from(connection_id.clone()))
                    .with_body(buffer[..read].to_vec());
                if outgoing.send(message).await.is_err() { break; }
            }
            changed = cancel.changed() => {
                if changed.is_err() || *cancel.borrow() { break; }
            }
        }
    }
    routes.lock().await.remove(&connection_id);
    let _ = outgoing
        .send(
            ProtoMessage::new(protocol, "stop")
                .with_session(session_id)
                .with_property(CONNECTION_ID, Value::from(connection_id)),
        )
        .await;
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
    let body = match rmp_serde::to_vec_named(&PortForwardOpen {
        remote_host: &mapping.remote_host,
        remote_port: mapping.remote_port,
        protocol: "udp",
    }) {
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
    loop {
        tokio::select! {
            message = route_rx.recv() => {
                let Some(message) = message else { break; };
                match message.header.message_type.as_str() {
                    "new" => started = true,
                    "forward" => {
                        if let Some(address) = source_address
                            && socket.send_to(&message.body, address).await.is_err()
                        {
                            break;
                        }
                        if protocol == PROTO_PORT_FORWARD {
                            let ack = ProtoMessage::new(protocol, "ack")
                                .with_session(&session_id)
                                .with_property(CONNECTION_ID, Value::from(connection_id.clone()));
                            if outgoing.send(ack).await.is_err() { break; }
                        }
                    }
                    "ack" if protocol == PROTO_PORT_FORWARD => ready_to_send = true,
                    "stop" => break,
                    _ => {}
                }
            }
            received = socket.recv_from(&mut buffer), if started && ready_to_send => {
                let Ok((read, address)) = received else { break; };
                source_address = Some(address);
                if protocol == PROTO_PORT_FORWARD { ready_to_send = false; }
                let message = ProtoMessage::new(protocol, "forward")
                    .with_session(&session_id)
                    .with_property(CONNECTION_ID, Value::from(connection_id.clone()))
                    .with_body(buffer[..read].to_vec());
                if outgoing.send(message).await.is_err() { break; }
            }
            changed = cancel.changed() => {
                if changed.is_err() || *cancel.borrow() { break; }
            }
        }
    }
    routes.lock().await.remove(&connection_id);
    let _ = outgoing
        .send(
            ProtoMessage::new(protocol, "stop")
                .with_session(session_id)
                .with_property(CONNECTION_ID, Value::from(connection_id)),
        )
        .await;
}

fn protocol_error(body: &[u8]) -> String {
    rmp_serde::from_slice::<ProtocolError>(body)
        .map(|error| error.error)
        .unwrap_or_else(|_| String::from_utf8_lossy(body).into_owned())
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
    use super::PortMapping;

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
}
