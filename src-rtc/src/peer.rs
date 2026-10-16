//! Reliable, ordered WebRTC DataChannel transport.

use std::{net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr}, sync::{Arc, Mutex as StdMutex}, time::{Duration, Instant}};

use bytes::BytesMut;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, mpsc, watch};
use webrtc::{
    data_channel::{DataChannel, DataChannelEvent, RTCDataChannelInit},
    peer_connection::{
        MediaEngine, PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler,
        RTCPeerConnectionIceEvent, RTCPeerConnectionIceErrorEvent, RTCConfigurationBuilder,
        RTCIceConnectionState, RTCIceGatheringState, RTCIceServer, RTCIceTransportPolicy, RTCStatsReportEntry, RTCConfiguration,
        RTCPeerConnectionState, RTCSessionDescription, StatsSelector,
    },
    runtime::{Runtime, TokioRuntime},
};

use crate::frame::{FrameDecoder, encode_frame};

const ICE_GATHER_TIMEOUT: Duration = Duration::from_secs(20);
const DATA_CHANNEL_TIMEOUT: Duration = Duration::from_secs(25);

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct IceServerConfig {
    pub urls: Vec<String>,
    pub username: Option<String>,
    pub credential: Option<String>,
}

#[derive(Clone)]
struct PeerEvents {
    gathering: watch::Sender<bool>,
    connected: watch::Sender<bool>,
    failed: watch::Sender<bool>,
    channels: mpsc::Sender<Arc<dyn DataChannel>>,
    candidate_counts: Arc<StdMutex<CandidateCounts>>,
}

#[derive(Default, Clone, Copy)]
struct CandidateCounts {
    host: u32,
    srflx: u32,
    relay: u32,
    prflx: u32,
}

impl CandidateCounts {
    fn record(&mut self, candidate_type: &str) {
        match candidate_type {
            "host" => self.host += 1,
            "srflx" => self.srflx += 1,
            "relay" => self.relay += 1,
            "prflx" => self.prflx += 1,
            _ => {}
        }
    }
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for PeerEvents {
    async fn on_ice_candidate(&self, event: RTCPeerConnectionIceEvent) {
        let candidate = event.candidate;
        let candidate_type = candidate.typ.to_string();
        if let Ok(mut counts) = self.candidate_counts.lock() {
            counts.record(&candidate_type);
        }
        if candidate_type == "relay" {
            log::debug!(
                "[rtc] relay candidate gathered: transport={} relay_transport={:?} address={}:{}",
                candidate.protocol,
                candidate.relay_protocol,
                candidate.address,
                candidate.port,
            );
        } else {
            log::debug!("[rtc] ICE candidate gathered: type={candidate_type} transport={}", candidate.protocol);
        }
    }

    async fn on_ice_candidate_error(&self, event: RTCPeerConnectionIceErrorEvent) {
        log::warn!("[rtc] ICE server candidate error: code={} detail={}", event.error_code, event.error_text);
    }

    async fn on_ice_connection_state_change(&self, state: RTCIceConnectionState) {
        log::info!("[rtc] ICE connection state: {state:?}");
    }

    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        log::debug!("[rtc] ICE gathering state: {state}");
        if state == RTCIceGatheringState::Complete {
            if let Ok(counts) = self.candidate_counts.lock() {
                log::info!(
                    "[rtc] ICE gathering complete: candidates host={} srflx={} relay={} prflx={}",
                    counts.host, counts.srflx, counts.relay, counts.prflx,
                );
            }
            self.gathering.send_replace(true);
        }
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        log::info!("[rtc] peer connection state: {state:?}");
        self.connected.send_replace(state == RTCPeerConnectionState::Connected);
        self.failed.send_replace(matches!(state, RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed));
    }

    async fn on_data_channel(&self, channel: Arc<dyn DataChannel>) {
        let _ = self.channels.try_send(channel);
    }
}

/// A negotiated peer with bounded JSON framing over an ordered reliable channel.
pub struct RtcPeer {
    connection: Box<dyn PeerConnection>,
    gathering_rx: watch::Receiver<bool>,
    connected_rx: watch::Receiver<bool>,
    failed_rx: watch::Receiver<bool>,
    channel_rx: mpsc::Receiver<Arc<dyn DataChannel>>,
    channel: Arc<Mutex<Option<Arc<dyn DataChannel>>>>,
    channel_open: watch::Receiver<bool>,
    channel_open_sender: watch::Sender<bool>,
    inbound: Mutex<mpsc::Receiver<Vec<u8>>>,
    inbound_sender: mpsc::Sender<Vec<u8>>,
    decoder: Mutex<FrameDecoder>,
    ice_servers: Mutex<Vec<IceServerConfig>>,
    force_relay: bool,
}

impl RtcPeer {
    pub async fn new(ice_servers: &[IceServerConfig], udp_addrs: Vec<String>, force_relay: bool) -> Result<Self, RtcError> {
        let ice_server_count = ice_servers.len();
        let udp_addrs = route_selected_udp_addrs(ice_servers, udp_addrs).await;
        let configuration = ice_configuration(ice_servers, force_relay)?;
        let (gathering_tx, gathering_rx) = watch::channel(false);
        let (connected_tx, connected_rx) = watch::channel(false);
        let (failed_tx, failed_rx) = watch::channel(false);
        let (channel_tx, channel_rx) = mpsc::channel(2);
        let candidate_counts = Arc::new(StdMutex::new(CandidateCounts::default()));
        let events = PeerEvents {
            gathering: gathering_tx,
            connected: connected_tx,
            failed: failed_tx,
            channels: channel_tx,
            candidate_counts: Arc::clone(&candidate_counts),
        };
        let mut media = MediaEngine::default();
        media.register_default_codecs().map_err(peer_error)?;
        let connection = PeerConnectionBuilder::new()
            .with_configuration(configuration)
            .with_media_engine(media)
            .with_handler(Arc::new(events.clone()))
            .with_runtime(Arc::new(TokioRuntime) as Arc<dyn Runtime>)
            .with_udp_addrs(udp_addrs)
            .build()
            .await
            .map(Box::new)
            .map_err(peer_error)?;
        log::info!("[rtc] PeerConnection created; force_relay={force_relay}; ICE servers={ice_server_count}");
        let (channel_open_tx, channel_open) = watch::channel(false);
        let (inbound_tx, inbound) = mpsc::channel(32);
        let peer = Self {
            connection,
            gathering_rx,
            connected_rx,
            failed_rx,
            channel_rx,
            channel: Arc::new(Mutex::new(None)),
            channel_open,
            channel_open_sender: channel_open_tx,
            inbound: Mutex::new(inbound),
            inbound_sender: inbound_tx,
            decoder: Mutex::new(FrameDecoder::default()),
            ice_servers: Mutex::new(ice_servers.to_vec()),
            force_relay,
        };
        Ok(peer)
    }

    /// Replaces the expiring TURN credentials without changing ICE URLs or transport policy.
    /// webrtc-rs recognizes this as credential rotation and refreshes existing allocations in place.
    pub async fn update_turn_credentials(&self, server: IceServerConfig) -> Result<(), RtcError> {
        if server.urls.is_empty() || server.username.as_deref().is_none_or(str::is_empty)
            || server.credential.as_deref().is_none_or(str::is_empty)
        {
            return Err(RtcError::Configuration("replacement TURN credentials are incomplete".into()));
        }
        let mut current = self.ice_servers.lock().await;
        let Some(index) = current.iter().position(|existing| existing.urls == server.urls) else {
            return Err(RtcError::Configuration("replacement TURN server URLs do not match the active ICE configuration".into()));
        };
        let mut next = current.clone();
        next[index] = server;
        let configuration = ice_configuration(&next, self.force_relay)?;
        self.connection.set_configuration(configuration).await.map_err(peer_error)?;
        *current = next;
        log::info!("[rtc] TURN credentials replaced for an active server; configuration update submitted to WebRTC");
        Ok(())
    }

    pub async fn create_offer(&mut self) -> Result<String, RtcError> {
        if self.channel.lock().await.is_none() {
            let channel = self.connection.create_data_channel(
                "shockingvrc-remote-v1",
                Some(RTCDataChannelInit {
                    ordered: true,
                    max_packet_life_time: None,
                    max_retransmits: None,
                    ..Default::default()
                }),
            ).await.map_err(peer_error)?;
            self.attach_channel(channel).await;
        }
        let offer = self.connection.create_offer(None).await.map_err(peer_error)?;
        self.connection.set_local_description(offer).await.map_err(peer_error)?;
        self.wait_gathering().await?;
        self.connection.local_description().await
            .map(|description| description.sdp)
            .ok_or_else(|| RtcError::Negotiation("local SDP was not produced".into()))
    }

    pub async fn accept_offer(&mut self, offer_sdp: &str) -> Result<String, RtcError> {
        let offer = RTCSessionDescription::offer(offer_sdp.to_owned()).map_err(peer_error)?;
        self.connection.set_remote_description(offer).await.map_err(peer_error)?;
        let answer = self.connection.create_answer(None).await.map_err(peer_error)?;
        self.connection.set_local_description(answer).await.map_err(peer_error)?;
        self.wait_gathering().await?;
        self.connection.local_description().await
            .map(|description| description.sdp)
            .ok_or_else(|| RtcError::Negotiation("local SDP was not produced".into()))
    }

    pub async fn set_answer(&mut self, answer_sdp: &str) -> Result<(), RtcError> {
        let answer = RTCSessionDescription::answer(answer_sdp.to_owned()).map_err(peer_error)?;
        self.connection.set_remote_description(answer).await.map_err(peer_error)
    }

    pub async fn wait_data_channel(&mut self) -> Result<(), RtcError> {
        if self.channel.lock().await.is_some() {
            return Ok(());
        }
        let channel = tokio::time::timeout(DATA_CHANNEL_TIMEOUT, self.channel_rx.recv())
            .await.map_err(|_| RtcError::Timeout("waiting for data channel"))?
            .ok_or(RtcError::Closed)?;
        self.attach_channel(channel).await;
        Ok(())
    }

    pub async fn wait_open(&mut self) -> Result<(), RtcError> {
        self.wait_data_channel().await?;
        let mut open = self.channel_open.clone();
        tokio::time::timeout(DATA_CHANNEL_TIMEOUT, async move {
            loop {
                if *open.borrow() { return Ok(()); }
                open.changed().await.map_err(|_| RtcError::Closed)?;
            }
        }).await.map_err(|_| RtcError::Timeout("opening data channel"))?
    }

    pub async fn wait_connected(&mut self) -> Result<(), RtcError> {
        let mut connected = self.connected_rx.clone();
        let mut failed = self.failed_rx.clone();
        tokio::time::timeout(DATA_CHANNEL_TIMEOUT, async move {
            loop {
                if *connected.borrow() { return Ok(()); }
                if *failed.borrow() { return Err(RtcError::Closed); }
                tokio::select! {
                    result = connected.changed() => result.map_err(|_| RtcError::Closed)?,
                    result = failed.changed() => result.map_err(|_| RtcError::Closed)?,
                }
            }
        }).await.map_err(|_| RtcError::Timeout("connecting ICE/DTLS"))?
    }

    pub async fn fingerprints(&self) -> Result<DtlsFingerprints, RtcError> {
        let report = self.connection.get_stats(Instant::now(), StatsSelector::None).await;
        let mut local_id = None;
        let mut remote_id = None;
        for entry in report.iter() {
            if let RTCStatsReportEntry::Transport(transport) = entry {
                if !transport.local_certificate_id.is_empty() { local_id = Some(transport.local_certificate_id.clone()); }
                if !transport.remote_certificate_id.is_empty() { remote_id = Some(transport.remote_certificate_id.clone()); }
            }
        }
        let mut local = None;
        let mut remote = None;
        for entry in report.iter() {
            if let RTCStatsReportEntry::Certificate(certificate) = entry {
                if Some(certificate.stats.id.as_str()) == local_id.as_deref() {
                    local = Some(normalize_dtls_fingerprint(&certificate.fingerprint, &certificate.fingerprint_algorithm)?);
                }
                if Some(certificate.stats.id.as_str()) == remote_id.as_deref() {
                    remote = Some(normalize_dtls_fingerprint(&certificate.fingerprint, &certificate.fingerprint_algorithm)?);
                }
            }
        }
        match (local, remote) {
            (Some(local), Some(remote)) => Ok(DtlsFingerprints { local, remote }),
            _ => Err(RtcError::FingerprintUnavailable),
        }
    }

    /// Returns only the selected candidate types, omitting local and public IP addresses.
    pub async fn selected_candidate_pair_label(&self) -> Option<String> {
        let sctp = self.connection.sctp().await?;
        let ice = sctp.transport().ice_transport();
        let pair = ice.get_selected_candidate_pair().await.ok().flatten()?;
        let local = pair.local();
        let remote = pair.remote();
        let label = candidate_pair_label(
            &local.typ.to_string(),
            &local.protocol.to_string(),
            &remote.typ.to_string(),
            &remote.protocol.to_string(),
            "succeeded",
        );
        log::info!(
            "[rtc] selected ICE pair: {label}; source=live_ice_transport",
        );
        log::debug!(
            "[rtc] selected ICE pair endpoints: local={}:{} remote={}:{}",
            local.address, local.port, remote.address, remote.port,
        );
        Some(label)
    }

    pub async fn send_json(&self, bytes: &[u8]) -> Result<(), RtcError> {
        let chunks = encode_frame(bytes).map_err(|error| RtcError::Frame(error.to_string()))?;
        let channel = self.channel.lock().await.clone().ok_or(RtcError::Closed)?;
        for chunk in chunks {
            channel.send(BytesMut::from(chunk.as_slice())).await.map_err(peer_error)?;
        }
        Ok(())
    }

    pub async fn recv_json(&self) -> Result<Vec<u8>, RtcError> {
        let mut inbound = self.inbound.lock().await;
        loop {
            let chunk = inbound.recv().await.ok_or(RtcError::Closed)?;
            if let Some(frame) = self.decoder.lock().await.push(&chunk).map_err(|error| RtcError::Frame(error.to_string()))? {
                return Ok(frame);
            }
        }
    }

    /// Bridges the RTC channel to bounded byte queues used by both the Host API and Terminal session loop.
    pub fn api_channels(self: &Arc<Self>) -> RtcApiChannels {
        let (inbound_tx, inbound) = mpsc::channel(32);
        let (outbound, mut outbound_rx) = mpsc::channel::<Vec<u8>>(32);
        let receive_peer = Arc::clone(self);
        let receive_task = tokio::spawn(async move {
            loop {
                match receive_peer.recv_json().await {
                    Ok(frame) => {
                        if inbound_tx.send(frame).await.is_err() {
                            log::info!("[rtc] DataChannel API receive pump stopped because its consumer closed");
                            break;
                        }
                    }
                    Err(error) => {
                        log::warn!("[rtc] DataChannel API receive pump ended: {error}");
                        break;
                    }
                }
            }
        });
        let send_peer = Arc::clone(self);
        let send_task = tokio::spawn(async move {
            while let Some(frame) = outbound_rx.recv().await {
                if let Err(error) = send_peer.send_json(&frame).await {
                    log::warn!("[rtc] DataChannel API send pump ended: {error}");
                    break;
                }
            }
        });
        RtcApiChannels { inbound: Some(inbound), outbound, tasks: vec![receive_task, send_task] }
    }

    pub async fn close(&self) {
        log::info!("[rtc] local peer close requested");
        if let Some(channel) = self.channel.lock().await.as_ref() {
            log::info!("[rtc] local DataChannel close requested");
            let _ = channel.close().await;
        }
        let _ = self.connection.close().await;
    }

    async fn wait_gathering(&mut self) -> Result<(), RtcError> {
        let mut gathering = self.gathering_rx.clone();
        tokio::time::timeout(ICE_GATHER_TIMEOUT, async move {
            loop {
                if *gathering.borrow() { return Ok(()); }
                gathering.changed().await.map_err(|_| RtcError::Closed)?;
            }
        }).await.map_err(|_| RtcError::Timeout("gathering ICE candidates"))?
    }

    async fn attach_channel(&mut self, channel: Arc<dyn DataChannel>) {
        log::info!("[rtc] DataChannel attached");
        *self.channel.lock().await = Some(channel.clone());
        let open_sender = self.channel_open_sender.clone();
        let inbound_sender = self.inbound_sender.clone();
        tokio::spawn(async move {
            while let Some(event) = channel.poll().await {
                match event {
                    DataChannelEvent::OnOpen => {
                        log::info!("[rtc] DataChannel opened");
                        open_sender.send_replace(true);
                    }
                    DataChannelEvent::OnMessage(message) => {
                        if inbound_sender.send(message.data.to_vec()).await.is_err() { break; }
                    }
                    DataChannelEvent::OnClose => {
                        log::info!("[rtc] DataChannel close event received; remote close reason is not exposed by the WebRTC API");
                        open_sender.send_replace(false);
                        break;
                    }
                    _ => {}
                }
            }
            log::debug!("[rtc] DataChannel event stream ended");
            open_sender.send_replace(false);
        });
    }

}

fn ice_transport_policy(force_relay: bool) -> RTCIceTransportPolicy {
    if force_relay { RTCIceTransportPolicy::Relay } else { RTCIceTransportPolicy::All }
}

/// Restricts wildcard ICE binds to the OS-selected outbound interfaces for the configured UDP
/// TURN servers. A wildcard otherwise expands to every adapter, including disconnected VM/VPN
/// adapters; ICE then waits for their independent TURN transactions before declaring gathering
/// complete. Explicit addresses and direct-only connections retain their configured behavior.
async fn route_selected_udp_addrs(ice_servers: &[IceServerConfig], udp_addrs: Vec<String>) -> Vec<String> {
    let wildcard_only = !udp_addrs.is_empty()
        && udp_addrs.iter().all(|addr| {
            addr.parse::<SocketAddr>()
                .is_ok_and(|socket_addr| socket_addr.ip().is_unspecified())
        });
    if !wildcard_only {
        return udp_addrs;
    }

    let mut endpoints = Vec::new();
    for server in ice_servers {
        for url in &server.urls {
            if let Some(endpoint) = parse_udp_turn_endpoint(url)
                && !endpoints.contains(&endpoint)
            {
                endpoints.push(endpoint);
            }
        }
    }
    if endpoints.is_empty() {
        return udp_addrs;
    }

    let mut local_ips = Vec::<IpAddr>::new();
    for (host, port) in endpoints {
        let Ok(remote_addrs) = tokio::net::lookup_host((host.as_str(), port)).await else {
            continue;
        };
        for remote in remote_addrs {
            let bind_addr = match remote.ip() {
                IpAddr::V4(_) => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
                IpAddr::V6(_) => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0),
            };
            let Ok(socket) = tokio::net::UdpSocket::bind(bind_addr).await else {
                continue;
            };
            if socket.connect(remote).await.is_err() {
                continue;
            }
            let Ok(local_addr) = socket.local_addr() else {
                continue;
            };
            let ip = local_addr.ip();
            let link_local = match ip {
                IpAddr::V4(ip) => ip.octets()[0] == 169 && ip.octets()[1] == 254,
                IpAddr::V6(ip) => ip.is_unicast_link_local(),
            };
            if ip.is_unspecified() || ip.is_loopback() || ip.is_multicast() || link_local {
                continue;
            }
            if !local_ips.contains(&ip) {
                local_ips.push(ip);
            }
        }
    }

    if local_ips.is_empty() {
        log::warn!("[rtc] could not select a local UDP route for TURN; retaining wildcard interface gathering");
        return udp_addrs;
    }

    log::info!("[rtc] selected {} OS-routed local UDP address(es) for TURN gathering", local_ips.len());
    local_ips
        .into_iter()
        .map(|ip| SocketAddr::new(ip, 0).to_string())
        .collect()
}

/// Returns a TURN UDP destination while discarding any URI userinfo before resolving its host.
fn parse_udp_turn_endpoint(value: &str) -> Option<(String, u16)> {
    let (scheme, rest) = value.split_once(':')?;
    if !scheme.eq_ignore_ascii_case("turn") {
        return None;
    }
    let (authority, query) = rest.split_once('?').unwrap_or((rest, ""));
    for parameter in query.split('&').filter(|part| !part.is_empty()) {
        if let Some((key, transport)) = parameter.split_once('=')
            && key.eq_ignore_ascii_case("transport")
            && !transport.eq_ignore_ascii_case("udp")
        {
            return None;
        }
    }
    let authority = authority.split('#').next()?;
    let authority = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    let parsed = url::Url::parse(&format!("udp://{authority}")).ok()?;
    let host = match parsed.host()? {
        url::Host::Domain(host) => host.to_owned(),
        url::Host::Ipv4(host) => host.to_string(),
        url::Host::Ipv6(host) => host.to_string(),
    };
    Some((host, parsed.port().unwrap_or(3478)))
}

fn ice_configuration(servers: &[IceServerConfig], force_relay: bool) -> Result<RTCConfiguration, RtcError> {
    let mut rtc_ice_servers = Vec::with_capacity(servers.len());
    for server in servers {
        if server.urls.is_empty() {
            return Err(RtcError::Configuration("ICE server requires at least one URI".into()));
        }
        rtc_ice_servers.push(RTCIceServer {
            urls: server.urls.clone(),
            username: server.username.clone().unwrap_or_default(),
            credential: server.credential.clone().unwrap_or_default(),
            ..Default::default()
        });
    }
    Ok(RTCConfigurationBuilder::new()
        .with_ice_servers(rtc_ice_servers)
        .with_ice_transport_policy(ice_transport_policy(force_relay))
        .build())
}

fn candidate_pair_label(
    local_type: &str,
    local_transport: &str,
    remote_type: &str,
    remote_transport: &str,
    state: &str,
) -> String {
    format!(
        "LOCAL {}/{} ↔ REMOTE {}/{} · {}",
        local_type.to_ascii_uppercase(),
        local_transport.to_ascii_uppercase(),
        remote_type.to_ascii_uppercase(),
        remote_transport.to_ascii_uppercase(),
        state.to_ascii_uppercase(),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        IceServerConfig, RtcPeer, candidate_pair_label, ice_transport_policy,
        parse_udp_turn_endpoint, route_selected_udp_addrs,
    };
    use webrtc::peer_connection::RTCIceTransportPolicy;

    #[test]
    fn force_relay_selects_relay_only_policy_and_default_keeps_all_candidates() {
        assert_eq!(ice_transport_policy(true), RTCIceTransportPolicy::Relay);
        assert_eq!(ice_transport_policy(false), RTCIceTransportPolicy::All);
    }

    #[test]
    fn selected_candidate_label_reports_types_without_addresses() {
        let label = candidate_pair_label("relay", "udp", "host", "udp", "succeeded");
        assert_eq!(label, "LOCAL RELAY/UDP ↔ REMOTE HOST/UDP · SUCCEEDED");
        assert!(!label.contains("192.0.2.10"));
        assert!(!label.contains("198.51.100.20"));
    }

    #[test]
    fn parses_udp_turn_endpoints_without_exposing_uri_credentials() {
        assert_eq!(
            parse_udp_turn_endpoint("turn:opaque-user@turn.example.test:3478?transport=udp"),
            Some(("turn.example.test".to_owned(), 3478)),
        );
        assert_eq!(
            parse_udp_turn_endpoint("turn:[2001:db8::1]:3479?transport=udp"),
            Some(("2001:db8::1".to_owned(), 3479)),
        );
        assert_eq!(
            parse_udp_turn_endpoint("turn:turn.example.test?transport=tcp"),
            None,
        );
    }

    #[tokio::test]
    async fn route_selection_preserves_explicit_and_direct_only_bind_addresses() {
        let configured = vec!["0.0.0.0:0".to_owned()];
        assert_eq!(
            route_selected_udp_addrs(&[], configured.clone()).await,
            configured,
        );

        let configured = vec!["127.0.0.1:0".to_owned()];
        let servers = vec![IceServerConfig {
            urls: vec!["turn:turn.example.test:3478?transport=udp".to_owned()],
            username: None,
            credential: None,
        }];
        assert_eq!(
            route_selected_udp_addrs(&servers, configured.clone()).await,
            configured,
        );
    }

    #[tokio::test]
    async fn connected_data_channel_exposes_selected_candidate_pair() {
        let bind = vec!["127.0.0.1:0".to_owned()];
        let mut terminal = RtcPeer::new(&[], bind.clone(), false)
            .await
            .expect("Terminal peer should build");
        let mut host = RtcPeer::new(&[], bind, false)
            .await
            .expect("Host peer should build");

        let offer = terminal.create_offer().await.expect("Terminal offer");
        let answer = host.accept_offer(&offer).await.expect("Host answer");
        terminal.set_answer(&answer).await.expect("Terminal applies answer");

        tokio::try_join!(terminal.wait_connected(), host.wait_connected())
            .expect("ICE and DTLS should connect over loopback");
        tokio::try_join!(terminal.wait_open(), host.wait_open())
            .expect("DataChannel should open over loopback");

        assert_eq!(
            terminal.selected_candidate_pair_label().await.as_deref(),
            Some("LOCAL HOST/UDP ↔ REMOTE HOST/UDP · SUCCEEDED"),
        );
        terminal.close().await;
        host.close().await;
    }
}

pub struct RtcApiChannels {
    inbound: Option<mpsc::Receiver<Vec<u8>>>,
    outbound: mpsc::Sender<Vec<u8>>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl RtcApiChannels {
    pub fn take(&mut self) -> Result<(mpsc::Receiver<Vec<u8>>, mpsc::Sender<Vec<u8>>), RtcError> {
        let inbound = self.inbound.take().ok_or(RtcError::Closed)?;
        Ok((inbound, self.outbound.clone()))
    }

    pub async fn close(mut self, peer: &RtcPeer) {
        self.inbound.take();
        drop(self.outbound);
        peer.close().await;
        for task in self.tasks.drain(..) {
            task.abort();
            let _ = task.await;
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DtlsFingerprints { pub local: [u8; 32], pub remote: [u8; 32] }

fn normalize_dtls_fingerprint(value: &str, algorithm: &str) -> Result<[u8; 32], RtcError> {
    if !algorithm.eq_ignore_ascii_case("sha-256") { return Err(RtcError::FingerprintAlgorithm); }
    let compact = value.replace(':', "");
    if compact.len() != 64 { return Err(RtcError::FingerprintUnavailable); }
    let mut bytes = [0_u8; 32];
    for (index, output) in bytes.iter_mut().enumerate() {
        let pair = compact.get(index * 2..index * 2 + 2).ok_or(RtcError::FingerprintUnavailable)?;
        *output = u8::from_str_radix(pair, 16).map_err(|_| RtcError::FingerprintUnavailable)?;
    }
    Ok(bytes)
}

fn peer_error(error: impl std::fmt::Display) -> RtcError { RtcError::WebRtc(error.to_string()) }

#[derive(Debug, thiserror::Error)]
pub enum RtcError {
    #[error("invalid ICE configuration: {0}")]
    Configuration(String),
    #[error("WebRTC operation failed: {0}")]
    WebRtc(String),
    #[error("WebRTC negotiation failed: {0}")]
    Negotiation(String),
    #[error("timed out while {0}")]
    Timeout(&'static str),
    #[error("WebRTC peer or data channel closed")]
    Closed,
    #[error("negotiated DTLS certificate fingerprint is unavailable")]
    FingerprintUnavailable,
    #[error("negotiated DTLS certificate did not use SHA-256")]
    FingerprintAlgorithm,
    #[error("invalid bounded JSON data frame: {0}")]
    Frame(String),
}
