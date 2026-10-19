use std::collections::BTreeMap;

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use native_tls::TlsConnector;
use rmpv::Value;
use serde::{Deserialize, Serialize};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    Connector, MaybeTlsStream, WebSocketStream, connect_async_tls_with_config,
    tungstenite::{Message, client::IntoClientRequest, http::HeaderValue},
};

pub const PROTO_SHELL: u16 = 1;
pub const PROTO_PORT_FORWARD: u16 = 3;
pub const PROTO_PORT_FORWARD_V2: u16 = PROTO_PORT_FORWARD + 0x100;
pub const PROTO_CONTROL: u16 = 0xFFFF;

pub type DeviceSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ProtoHeader {
    #[serde(rename = "proto")]
    pub protocol: u16,
    #[serde(rename = "typ", default, skip_serializing_if = "String::is_empty")]
    pub message_type: String,
    #[serde(rename = "sid", default, skip_serializing_if = "String::is_empty")]
    pub session_id: String,
    #[serde(rename = "props", default, skip_serializing_if = "BTreeMap::is_empty")]
    pub properties: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ProtoMessage {
    #[serde(rename = "hdr")]
    pub header: ProtoHeader,
    #[serde(
        rename = "body",
        default,
        skip_serializing_if = "Vec::is_empty",
        with = "serde_bytes"
    )]
    pub body: Vec<u8>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Open {
    pub versions: Vec<u8>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Accept {
    pub version: u8,
    pub protocols: Vec<u16>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ProtocolError {
    #[serde(rename = "err")]
    pub error: String,
    #[serde(default)]
    pub close: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PortForwardOpen<'a> {
    pub remote_host: &'a str,
    pub remote_port: u16,
    pub protocol: &'a str,
}

impl ProtoMessage {
    pub fn new(protocol: u16, message_type: impl Into<String>) -> Self {
        Self {
            header: ProtoHeader {
                protocol,
                message_type: message_type.into(),
                ..ProtoHeader::default()
            },
            body: Vec::new(),
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        Ok(rmp_serde::to_vec_named(self)?)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        Ok(rmp_serde::from_slice(bytes)?)
    }

    pub fn with_session(mut self, session_id: impl Into<String>) -> Self {
        self.header.session_id = session_id.into();
        self
    }

    pub fn with_body(mut self, body: Vec<u8>) -> Self {
        self.body = body;
        self
    }

    pub fn with_property(mut self, name: impl Into<String>, value: Value) -> Self {
        self.header.properties.insert(name.into(), value);
        self
    }

    pub fn string_property(&self, name: &str) -> Option<String> {
        self.header
            .properties
            .get(name)
            .and_then(|value| value.as_str())
            .map(ToOwned::to_owned)
    }
}

pub async fn connect_device(
    server: &str,
    device_id: &str,
    token: &str,
    skip_verify: bool,
) -> Result<DeviceSocket> {
    let mut url = url::Url::parse(server)?;
    url.set_scheme(if url.scheme() == "https" { "wss" } else { "ws" })
        .map_err(|_| anyhow::anyhow!("server scheme is not compatible with WebSocket"))?;
    url.set_path(&format!(
        "/api/management/v1/deviceconnect/devices/{device_id}/connect"
    ));
    url.set_query(None);
    let mut request = url.as_str().into_client_request()?;
    request.headers_mut().insert(
        "Authorization",
        HeaderValue::from_str(&format!("Bearer {token}"))?,
    );
    let connector = if url.scheme() == "wss" {
        Some(Connector::NativeTls(
            TlsConnector::builder()
                .danger_accept_invalid_certs(skip_verify)
                .danger_accept_invalid_hostnames(skip_verify)
                .build()?,
        ))
    } else {
        None
    };
    let (socket, _) = connect_async_tls_with_config(request, None, false, connector).await?;
    Ok(socket)
}

pub async fn send_proto<S>(sink: &mut S, message: ProtoMessage) -> Result<()>
where
    S: futures_util::Sink<Message> + Unpin,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    sink.send(Message::Binary(message.encode()?.into())).await?;
    Ok(())
}

pub async fn next_proto<S>(stream: &mut S) -> Result<Option<ProtoMessage>>
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    while let Some(message) = stream.next().await {
        match message? {
            Message::Binary(bytes) => return Ok(Some(ProtoMessage::decode(&bytes)?)),
            Message::Close(_) => return Ok(None),
            _ => {}
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proto_message_round_trips_as_message_pack() {
        let message = ProtoMessage::new(PROTO_SHELL, "shell")
            .with_session("session")
            .with_property("timeout", Value::from(30))
            .with_body(b"hello".to_vec());
        let decoded = ProtoMessage::decode(&message.encode().unwrap()).unwrap();
        assert_eq!(decoded.header.protocol, PROTO_SHELL);
        assert_eq!(decoded.header.message_type, "shell");
        assert_eq!(decoded.header.session_id, "session");
        assert_eq!(decoded.body, b"hello");
    }
}
