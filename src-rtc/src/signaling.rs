//! TLS-verified client for the v1 self-hosted rendezvous contract.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::{net::TcpStream, time::timeout};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async_with_config,
    tungstenite::{Message, client::IntoClientRequest, protocol::WebSocketConfig},
};
use url::Url;
use uuid::Uuid;

use shocking_vrc_rendezvous::protocol::{SIGNAL_PROTOCOL, SignalMessage};

const SIGNAL_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_SIGNAL_FRAME_BYTES: usize = 64 * 1024;

type SignalWebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// An outbound signal connection. `ws://` is accepted only for loopback proof/development;
/// non-loopback rendezvous URLs must use TLS (`wss://`).
pub struct SignalSocket {
    socket: SignalWebSocket,
}

impl SignalSocket {
    pub async fn connect(url: &str) -> Result<Self, SignalError> {
        validate_signal_url(url)?;
        let request = url.into_client_request().map_err(|error| SignalError::Url(error.to_string()))?;
        let mut config = WebSocketConfig::default();
        config.max_message_size = Some(MAX_SIGNAL_FRAME_BYTES);
        config.max_frame_size = Some(MAX_SIGNAL_FRAME_BYTES);
        let (socket, _) = timeout(SIGNAL_TIMEOUT, connect_async_with_config(request, Some(config), false))
            .await.map_err(|_| SignalError::Timeout("connecting to rendezvous"))?
            .map_err(|error| SignalError::Transport(error.to_string()))?;
        Ok(Self { socket })
    }

    pub async fn send(&mut self, message: &SignalMessage) -> Result<(), SignalError> {
        message.validate().map_err(|error| SignalError::Protocol(error.to_string()))?;
        let encoded = serde_json::to_string(message).map_err(|error| SignalError::Protocol(error.to_string()))?;
        if encoded.len() > MAX_SIGNAL_FRAME_BYTES {
            return Err(SignalError::Protocol("signaling frame exceeds the v1 limit".into()));
        }
        timeout(SIGNAL_TIMEOUT, self.socket.send(Message::Text(encoded.into())))
            .await.map_err(|_| SignalError::Timeout("sending rendezvous message"))?
            .map_err(|error| SignalError::Transport(error.to_string()))
    }

    pub async fn receive(&mut self) -> Result<SignalMessage, SignalError> {
        loop {
            let next = timeout(SIGNAL_TIMEOUT, self.socket.next())
                .await.map_err(|_| SignalError::Timeout("waiting for rendezvous message"))?;
            match next {
                Some(Ok(Message::Text(text))) => {
                    if text.len() > MAX_SIGNAL_FRAME_BYTES { return Err(SignalError::Protocol("rendezvous frame exceeds the v1 limit".into())); }
                    let message: SignalMessage = serde_json::from_str(text.as_str())
                        .map_err(|error| SignalError::Protocol(error.to_string()))?;
                    message.validate().map_err(|error| SignalError::Protocol(error.to_string()))?;
                    return Ok(message);
                }
                Some(Ok(Message::Ping(payload))) => {
                    self.socket.send(Message::Pong(payload)).await.map_err(|error| SignalError::Transport(error.to_string()))?;
                }
                Some(Ok(Message::Pong(_))) => {}
                Some(Ok(Message::Close(_))) | None => return Err(SignalError::Closed),
                Some(Ok(Message::Binary(_))) => return Err(SignalError::Protocol("rendezvous accepts JSON text frames only".into())),
                Some(Ok(Message::Frame(_))) => {}
                Some(Err(error)) => return Err(SignalError::Transport(error.to_string())),
            }
        }
    }

    pub async fn close(&mut self) {
        let _ = self.socket.send(Message::Close(None)).await;
    }
}

pub fn validate_signal_url(raw: &str) -> Result<Url, SignalError> {
    let url = Url::parse(raw).map_err(|error| SignalError::Url(error.to_string()))?;
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err(SignalError::Url("rendezvous URL cannot contain credentials or a fragment".into()));
    }
    match url.scheme() {
        "wss" => Ok(url),
        "ws" if is_loopback_host(&url) => Ok(url),
        "ws" => Err(SignalError::Url("non-loopback rendezvous requires wss://".into())),
        _ => Err(SignalError::Url("rendezvous URL must use wss:// (or ws:// on loopback)".into())),
    }
}

fn is_loopback_host(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    }
}

pub fn register_message(host_id: String, identity_public_key: String, registration_key: String) -> SignalMessage {
    SignalMessage::Register {
        protocol: SIGNAL_PROTOCOL,
        request_id: Uuid::new_v4(),
        host_id,
        identity_public_key,
        authorization: registration_key,
    }
}

pub fn join_message(host_id: String, invite_token: String) -> SignalMessage {
    SignalMessage::Join {
        protocol: SIGNAL_PROTOCOL,
        request_id: Uuid::new_v4(),
        host_id,
        authorization: invite_token,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SignalError {
    #[error("invalid rendezvous URL: {0}")]
    Url(String),
    #[error("rendezvous connection failed: {0}")]
    Transport(String),
    #[error("invalid rendezvous message: {0}")]
    Protocol(String),
    #[error("timed out while {0}")]
    Timeout(&'static str),
    #[error("rendezvous closed the signaling connection")]
    Closed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permits_tls_urls_and_loopback_development_only() {
        assert!(validate_signal_url("wss://relay.example:443/signal").is_ok());
        assert!(validate_signal_url("ws://localhost:8080/signal").is_ok());
        assert!(validate_signal_url("ws://127.0.0.1:8080/signal").is_ok());
        assert!(validate_signal_url("ws://relay.example/signal").is_err());
        assert!(validate_signal_url("https://relay.example/signal").is_err());
    }
}
