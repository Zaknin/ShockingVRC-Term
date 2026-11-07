//! Explicit, bounded connectivity probes for the Host setup page.
//!
//! These checks do not register a Host, create a signaling room, request TURN
//! credentials, or start an ICE session. They run only when the UI calls
//! [`probe_local_services`].

use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use ring::rand::{SecureRandom, SystemRandom};
use rustls::{ClientConfig, RootCertStore, pki_types::ServerName};
use tokio::{
    net::{TcpSocket, TcpStream, UdpSocket, lookup_host},
    time::timeout,
};
use tokio_rustls::TlsConnector;
use tokio_tungstenite::{
    client_async,
    tungstenite::{Error as WebSocketError, client::IntoClientRequest},
};

const DNS_TIMEOUT: Duration = Duration::from_secs(3);
const TCP_TIMEOUT: Duration = Duration::from_secs(3);
const TLS_TIMEOUT: Duration = Duration::from_secs(4);
const WS_TIMEOUT: Duration = Duration::from_secs(3);
const STUN_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_STUN_PROBES: usize = 4;
const STUN_COOKIE: u32 = 0x2112_A442;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeLine {
    pub component: &'static str,
    pub code: &'static str,
    pub detail: &'static str,
}

impl ProbeLine {
    fn new(component: &'static str, code: &'static str, detail: &'static str) -> Self {
        Self {
            component,
            code,
            detail,
        }
    }

    pub fn display_line(&self) -> String {
        format!("{}: {} · {}", self.component, self.code, self.detail)
    }
}

/// Probes the configured Rendezvous WebSocket endpoint and configured UDP STUN
/// endpoints on explicit invocation. `bind_address` is optional; when selected,
/// TCP and UDP probes bind to that local interface address. No interface is
/// selected automatically.
pub async fn probe_local_services(
    rendezvous_url: &str,
    ice_server_urls: &[String],
    bind_address: Option<IpAddr>,
) -> Vec<ProbeLine> {
    let mut report = vec![probe_rendezvous_websocket(rendezvous_url, bind_address).await];

    let stun_urls = ice_server_urls
        .iter()
        .filter(|url| url.starts_with("stun:") || url.starts_with("stuns:"))
        .take(MAX_STUN_PROBES)
        .collect::<Vec<_>>();
    if stun_urls.is_empty() {
        report.push(ProbeLine::new("STUN", "NOT_CONFIGURED", "no STUN URI is configured"));
    } else {
        for url in stun_urls {
            report.push(probe_stun_binding(url, bind_address).await);
        }
    }
    if ice_server_urls
        .iter()
        .filter(|url| url.starts_with("stun:") || url.starts_with("stuns:"))
        .count()
        > MAX_STUN_PROBES
    {
        report.push(ProbeLine::new(
            "STUN",
            "LIMIT_REACHED",
            "only the first four configured STUN URIs were probed",
        ));
    }

    report.push(ProbeLine::new(
        "TURN",
        "TURN_ALLOCATION_NOT_RUN",
        "Allocate requires short-lived credentials from an authorized room; no unauthenticated request was sent",
    ));
    report.push(ProbeLine::new(
        "ICE",
        "ICE_PAIR_NOT_TESTED",
        "this probe does not start peer negotiation or claim a selected candidate pair",
    ));
    report
}

async fn probe_rendezvous_websocket(value: &str, bind_address: Option<IpAddr>) -> ProbeLine {
    let Ok(url) = shocking_vrc_rtc::validate_signal_url(value) else {
        return ProbeLine::new("RENDEZVOUS", "INVALID_URL", "enter a valid ws:// or wss:// URL");
    };
    if !matches!(url.scheme(), "ws" | "wss")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return ProbeLine::new(
            "RENDEZVOUS",
            "INVALID_URL",
            "only ws:// or wss:// endpoints without embedded credentials are supported",
        );
    }
    let host = url.host_str().unwrap_or_default().to_owned();
    let Some(port) = url.port_or_known_default() else {
        return ProbeLine::new("RENDEZVOUS", "INVALID_URL", "the endpoint has no usable port");
    };
    let resolved = match timeout(DNS_TIMEOUT, lookup_host((host.as_str(), port))).await {
        Err(_) => return ProbeLine::new("RENDEZVOUS", "DNS_TIMEOUT", "name resolution timed out"),
        Ok(Err(_)) => return ProbeLine::new("RENDEZVOUS", "DNS", "name resolution failed"),
        Ok(Ok(addresses)) => addresses.collect::<Vec<_>>(),
    };
    if resolved.is_empty() {
        return ProbeLine::new("RENDEZVOUS", "DNS", "name resolution returned no addresses");
    }

    let stream = match connect_tcp(&resolved, bind_address).await {
        Ok(stream) => stream,
        Err(code) => return ProbeLine::new("RENDEZVOUS", code, "TCP connection could not be established"),
    };
    let request = match url.as_str().into_client_request() {
        Ok(request) => request,
        Err(_) => return ProbeLine::new("RENDEZVOUS", "INVALID_URL", "the WebSocket request could not be built"),
    };

    if url.scheme() == "wss" {
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = match ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
        {
            Ok(builder) => builder.with_root_certificates(roots).with_no_client_auth(),
            Err(_) => {
                return ProbeLine::new(
                    "RENDEZVOUS",
                    "LOCAL_ERROR",
                    "TLS client configuration could not be initialized",
                );
            }
        };
        let Ok(server_name) = ServerName::try_from(host.clone()) else {
            return ProbeLine::new("RENDEZVOUS", "TLS", "TLS server name is invalid");
        };
        let tls = match timeout(TLS_TIMEOUT, TlsConnector::from(Arc::new(config)).connect(server_name, stream)).await {
            Err(_) => return ProbeLine::new("RENDEZVOUS", "TLS_TIMEOUT", "TLS handshake timed out"),
            Ok(Err(_)) => return ProbeLine::new("RENDEZVOUS", "TLS", "TLS handshake or certificate validation failed"),
            Ok(Ok(stream)) => stream,
        };
        match timeout(WS_TIMEOUT, client_async(request, tls)).await {
            Err(_) => ProbeLine::new("RENDEZVOUS", "WS_TIMEOUT", "WebSocket upgrade timed out"),
            Ok(Err(WebSocketError::Http(_) | WebSocketError::HttpFormat(_))) => ProbeLine::new(
                "RENDEZVOUS",
                "HTTP",
                "TLS succeeded but the HTTP WebSocket upgrade was rejected",
            ),
            Ok(Err(_)) => ProbeLine::new("RENDEZVOUS", "WS", "TLS succeeded but the WebSocket upgrade failed"),
            Ok(Ok((socket, _response))) => {
                drop(socket);
                ProbeLine::new(
                    "RENDEZVOUS",
                    "WSS_OK",
                    "TLS and WebSocket upgrade succeeded; registration was not tested",
                )
            }
        }
    } else {
        match timeout(WS_TIMEOUT, client_async(request, stream)).await {
            Err(_) => ProbeLine::new("RENDEZVOUS", "WS_TIMEOUT", "WebSocket upgrade timed out"),
            Ok(Err(WebSocketError::Http(_) | WebSocketError::HttpFormat(_))) => ProbeLine::new(
                "RENDEZVOUS",
                "HTTP",
                "TCP succeeded but the HTTP WebSocket upgrade was rejected",
            ),
            Ok(Err(_)) => ProbeLine::new("RENDEZVOUS", "WS", "TCP succeeded but the WebSocket upgrade failed"),
            Ok(Ok((socket, _response))) => {
                drop(socket);
                ProbeLine::new(
                    "RENDEZVOUS",
                    "WS_OK",
                    "WebSocket upgrade succeeded; registration was not tested",
                )
            }
        }
    }
}

async fn connect_tcp(addresses: &[SocketAddr], bind_address: Option<IpAddr>) -> Result<TcpStream, &'static str> {
    let mut saw_compatible_address = false;
    let mut timed_out = false;
    for address in addresses {
        if bind_address.is_some_and(|local| local.is_ipv4() != address.is_ipv4()) {
            continue;
        }
        saw_compatible_address = true;
        let socket = if address.is_ipv4() {
            TcpSocket::new_v4()
        } else {
            TcpSocket::new_v6()
        }
        .map_err(|_| "TCP")?;
        if let Some(local_address) = bind_address {
            socket
                .bind(SocketAddr::new(local_address, 0))
                .map_err(|_| "INTERFACE")?;
        }
        match timeout(TCP_TIMEOUT, socket.connect(*address)).await {
            Err(_) => timed_out = true,
            Ok(Err(_)) => {}
            Ok(Ok(stream)) => return Ok(stream),
        }
    }
    if !saw_compatible_address {
        Err("INTERFACE")
    } else if timed_out {
        Err("TCP_TIMEOUT")
    } else {
        Err("TCP")
    }
}

async fn probe_stun_binding(value: &str, bind_address: Option<IpAddr>) -> ProbeLine {
    let Some((host, port)) = parse_stun_udp_uri(value) else {
        let code = if value.starts_with("stuns:") {
            "UNSUPPORTED"
        } else {
            "INVALID_URI"
        };
        return ProbeLine::new("STUN", code, "only stun:host[:port] UDP Binding checks are supported");
    };
    let resolved = match timeout(DNS_TIMEOUT, lookup_host((host.as_str(), port))).await {
        Err(_) => return ProbeLine::new("STUN", "DNS_TIMEOUT", "name resolution timed out"),
        Ok(Err(_)) => return ProbeLine::new("STUN", "DNS", "name resolution failed"),
        Ok(Ok(addresses)) => addresses.collect::<Vec<_>>(),
    };
    if resolved.is_empty() {
        return ProbeLine::new("STUN", "DNS", "name resolution returned no addresses");
    }

    let mut saw_compatible_address = false;
    for remote in resolved {
        if bind_address.is_some_and(|local| local.is_ipv4() != remote.is_ipv4()) {
            continue;
        }
        saw_compatible_address = true;
        let bind_ip = bind_address.unwrap_or_else(|| {
            if remote.is_ipv4() {
                IpAddr::V4(Ipv4Addr::UNSPECIFIED)
            } else {
                IpAddr::V6(Ipv6Addr::UNSPECIFIED)
            }
        });
        let socket = match UdpSocket::bind(SocketAddr::new(bind_ip, 0)).await {
            Ok(socket) => socket,
            Err(_) => {
                return ProbeLine::new(
                    "STUN",
                    "INTERFACE",
                    "UDP socket could not bind to the selected local interface",
                );
            }
        };
        let mut transaction_id = [0_u8; 12];
        if SystemRandom::new().fill(&mut transaction_id).is_err() {
            return ProbeLine::new("STUN", "LOCAL_ERROR", "secure transaction ID generation failed");
        }
        let request = stun_binding_request(transaction_id);
        if socket.send_to(&request, remote).await.is_err() {
            continue;
        }
        let mut response = [0_u8; 1200];
        let received = timeout(STUN_TIMEOUT, socket.recv_from(&mut response)).await;
        let Ok(Ok((length, source))) = received else { continue };
        if source == remote && xor_mapped_address(&response[..length], transaction_id).is_some() {
            return ProbeLine::new(
                "STUN",
                "STUN_BINDING",
                "transaction matched and XOR-MAPPED-ADDRESS was present; address omitted",
            );
        }
    }
    if !saw_compatible_address {
        return ProbeLine::new(
            "STUN",
            "INTERFACE",
            "no resolved address matches the selected local interface family",
        );
    }
    ProbeLine::new(
        "STUN",
        "STUN_NO_REPLY",
        "no valid Binding response with a matching transaction and XOR-MAPPED-ADDRESS",
    )
}

fn parse_stun_udp_uri(value: &str) -> Option<(String, u16)> {
    let rest = value.strip_prefix("stun:")?;
    let (without_query, query) = rest.split_once('?').unwrap_or((rest, ""));
    if !query.is_empty() && !query.split('&').any(|part| part.eq_ignore_ascii_case("transport=udp")) {
        return None;
    }
    let authority = without_query
        .strip_prefix("//")
        .unwrap_or(without_query)
        .split(['/', '#'])
        .next()?;
    if authority.is_empty() || authority.chars().any(char::is_whitespace) {
        return None;
    }
    if authority.starts_with('[') {
        let end = authority.find(']')?;
        let host = authority.get(1..end)?;
        let suffix = authority.get(end + 1..)?;
        let port = if suffix.is_empty() {
            3478
        } else {
            suffix.strip_prefix(':')?.parse().ok()?
        };
        return (!host.is_empty() && port > 0).then(|| (host.to_owned(), port));
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') => (host, port.parse().ok()?),
        Some(_) => return None,
        None => (authority, 3478),
    };
    (!host.is_empty() && !host.contains('@') && port > 0).then(|| (host.to_owned(), port))
}

fn stun_binding_request(transaction_id: [u8; 12]) -> [u8; 20] {
    let mut request = [0_u8; 20];
    request[..2].copy_from_slice(&0x0001_u16.to_be_bytes());
    request[4..8].copy_from_slice(&STUN_COOKIE.to_be_bytes());
    request[8..].copy_from_slice(&transaction_id);
    request
}

fn xor_mapped_address(message: &[u8], transaction_id: [u8; 12]) -> Option<SocketAddr> {
    if message.len() < 20
        || message[..2] != 0x0101_u16.to_be_bytes()
        || message[2..4] != ((message.len() - 20) as u16).to_be_bytes()
        || message[4..8] != STUN_COOKIE.to_be_bytes()
        || message[8..20] != transaction_id
    {
        return None;
    }
    let mut offset = 20;
    while offset + 4 <= message.len() {
        let attribute = u16::from_be_bytes([message[offset], message[offset + 1]]);
        let length = u16::from_be_bytes([message[offset + 2], message[offset + 3]]) as usize;
        let start = offset + 4;
        let end = start.saturating_add(length);
        if end > message.len() {
            return None;
        }
        if attribute == 0x0020 {
            let data = message.get(start..end)?;
            if data.len() < 4 || data[0] != 0 {
                return None;
            }
            let port = u16::from_be_bytes([data[2], data[3]]) ^ ((STUN_COOKIE >> 16) as u16);
            if port == 0 {
                return None;
            }
            return match data[1] {
                0x01 if data.len() == 8 => {
                    let cookie = STUN_COOKIE.to_be_bytes();
                    let address = Ipv4Addr::new(
                        data[4] ^ cookie[0],
                        data[5] ^ cookie[1],
                        data[6] ^ cookie[2],
                        data[7] ^ cookie[3],
                    );
                    Some(SocketAddr::new(IpAddr::V4(address), port))
                }
                0x02 if data.len() == 20 => {
                    let mut mask = [0_u8; 16];
                    mask[..4].copy_from_slice(&STUN_COOKIE.to_be_bytes());
                    mask[4..].copy_from_slice(&transaction_id);
                    let mut address = [0_u8; 16];
                    for (index, byte) in address.iter_mut().enumerate() {
                        *byte = data[4 + index] ^ mask[index];
                    }
                    Some(SocketAddr::new(IpAddr::V6(Ipv6Addr::from(address)), port))
                }
                _ => None,
            };
        }
        offset = end.saturating_add((4 - (length % 4)) % 4);
    }
    None
}

#[cfg(test)]
mod tests {
    use std::{
        net::{IpAddr, Ipv4Addr},
        time::Duration,
    };

    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, UdpSocket},
        time::timeout,
    };
    use tokio_tungstenite::accept_async;

    use super::{parse_stun_udp_uri, probe_local_services, stun_binding_request, xor_mapped_address};

    #[test]
    fn stun_uri_parser_accepts_ipv4_ipv6_and_default_port() {
        assert_eq!(
            parse_stun_udp_uri("stun:stun.example.test"),
            Some(("stun.example.test".into(), 3478))
        );
        assert_eq!(parse_stun_udp_uri("stun:host.test:5349"), Some(("host.test".into(), 5349)));
        assert_eq!(parse_stun_udp_uri("stun:[::1]:3479"), Some(("::1".into(), 3479)));
        assert_eq!(parse_stun_udp_uri("stun:::1"), None);
        assert_eq!(parse_stun_udp_uri("stuns:host.test:5349"), None);
    }

    #[test]
    fn binding_success_requires_matching_cookie_transaction_and_xor_mapped_shape() {
        let transaction_id = [0x5a; 12];
        let request = stun_binding_request(transaction_id);
        let mut response = Vec::from([0x01, 0x01, 0x00, 0x0c, 0x21, 0x12, 0xa4, 0x42]);
        response.extend_from_slice(&transaction_id);
        response.extend_from_slice(&[0x00, 0x20, 0x00, 0x08, 0x00, 0x01, 0x11, 0x2b, 0x5e, 0x53, 0x95, 0x8f]);
        assert_eq!(request.len(), 20);
        assert!(xor_mapped_address(&response, transaction_id).is_some());
        assert!(xor_mapped_address(&response, [0x33; 12]).is_none());
        response[23] = 0x07;
        assert!(xor_mapped_address(&response, transaction_id).is_none());
    }

    #[tokio::test]
    async fn local_websocket_probe_checks_only_the_upgrade_and_sends_no_registration() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let message = timeout(Duration::from_millis(150), futures_util::StreamExt::next(&mut socket)).await;
            let sent_application_frame = matches!(
                message,
                Ok(Some(Ok(tokio_tungstenite::tungstenite::Message::Text(_)
                    | tokio_tungstenite::tungstenite::Message::Binary(_))))
            );
            assert!(!sent_application_frame, "probe must not register or send a signaling message");
        });

        let report =
            probe_local_services(&format!("ws://{address}/signal"), &[], Some(IpAddr::V4(Ipv4Addr::LOCALHOST))).await;

        assert_eq!(report[0].code, "WS_OK");
        assert_eq!(report[1].code, "NOT_CONFIGURED");
        assert_eq!(report[2].code, "TURN_ALLOCATION_NOT_RUN");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn local_websocket_probe_distinguishes_http_upgrade_rejection() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).await.unwrap();
            stream
                .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
        });

        let report =
            probe_local_services(&format!("ws://{address}/signal"), &[], Some(IpAddr::V4(Ipv4Addr::LOCALHOST))).await;

        assert_eq!(report[0].code, "HTTP");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn local_wss_probe_reports_untrusted_certificate_as_tls_failure() {
        let certificate = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
        let certificate_der = CertificateDer::from(certificate.cert.der().to_vec());
        let private_key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(certificate.signing_key.serialize_der()));
        let server_config =
            rustls::ServerConfig::builder_with_provider(std::sync::Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(vec![certificate_der], private_key)
                .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(server_config));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let _ = acceptor.accept(stream).await;
        });

        let report = probe_local_services(
            &format!("wss://localhost:{port}/signal"),
            &[],
            Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        )
        .await;

        assert_eq!(report[0].code, "TLS");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn local_stun_probe_checks_transaction_and_xor_mapped_address() {
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = server.local_addr().unwrap();
        let server_task = tokio::spawn(async move {
            let mut request = [0_u8; 64];
            let (length, client) = server.recv_from(&mut request).await.unwrap();
            assert_eq!(length, 20);
            let mut response = Vec::from([0x01, 0x01, 0x00, 0x0c, 0x21, 0x12, 0xa4, 0x42]);
            response.extend_from_slice(&request[8..20]);
            let xor_port = 43210_u16 ^ 0x2112;
            let xor_ip = [127_u8 ^ 0x21, 0 ^ 0x12, 0 ^ 0xa4, 1 ^ 0x42];
            response.extend_from_slice(&[0x00, 0x20, 0x00, 0x08, 0x00, 0x01]);
            response.extend_from_slice(&xor_port.to_be_bytes());
            response.extend_from_slice(&xor_ip);
            assert_eq!(
                xor_mapped_address(&response, request[8..20].try_into().unwrap()),
                Some("127.0.0.1:43210".parse().unwrap())
            );
            server.send_to(&response, client).await.unwrap();
        });

        let report = probe_local_services(
            "ws://127.0.0.1:1/unused",
            &[format!("stun:127.0.0.1:{}", address.port())],
            Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        )
        .await;

        assert_eq!(report[0].code, "TCP");
        assert_eq!(report[1].code, "STUN_BINDING");
        assert!(!report[1].display_line().contains("127.0.0.1"));
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn stun_probe_reports_timeout_for_a_wrong_transaction_id() {
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = server.local_addr().unwrap();
        tokio::spawn(async move {
            let mut request = [0_u8; 64];
            let (length, client) = server.recv_from(&mut request).await.unwrap();
            let wrong_transaction = [0x55; 12];
            let mut response = Vec::from([0x01, 0x01, 0x00, 0x0c, 0x21, 0x12, 0xa4, 0x42]);
            response.extend_from_slice(&wrong_transaction);
            response.extend_from_slice(&[0x00, 0x20, 0x00, 0x08, 0x00, 0x01, 0x21, 0x12, 0x5e, 0x53, 0x95, 0x8f]);
            assert_eq!(length, 20);
            server.send_to(&response, client).await.unwrap();
        });

        let report = probe_local_services(
            "ws://127.0.0.1:1/unused",
            &[format!("stun:127.0.0.1:{}", address.port())],
            Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        )
        .await;
        assert_eq!(report[1].code, "STUN_NO_REPLY");
    }
}
