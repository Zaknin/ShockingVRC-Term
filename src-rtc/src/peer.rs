//! Reliable, ordered WebRTC DataChannel transport.

use std::{
    collections::HashSet,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use bytes::BytesMut;
use serde::{Deserialize, Serialize};
use shocking_vrc_rendezvous::protocol::normalize_ice_server_uri;
use tokio::sync::{Mutex, mpsc, watch};
use webrtc::{
    data_channel::{DataChannel, DataChannelEvent, RTCDataChannelInit},
    peer_connection::{
        MediaEngine, PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCConfiguration,
        RTCConfigurationBuilder, RTCIceCandidateType, RTCIceConnectionState, RTCIceGatheringState, RTCIceServer,
        RTCIceTransportPolicy, RTCPeerConnectionIceErrorEvent, RTCPeerConnectionIceEvent, RTCPeerConnectionState,
        RTCSessionDescription, RTCStatsReportEntry, StatsSelector,
    },
    runtime::{Runtime, TokioRuntime},
};

use crate::diagnostics::monotonic_ms;
use crate::frame::{FrameDecoder, encode_frame};

const ICE_GATHER_TIMEOUT: Duration = Duration::from_secs(20);
const DATA_CHANNEL_TIMEOUT: Duration = Duration::from_secs(25);
const DATA_CHANNEL_HIGH_WATER_BYTES: u32 = 256 * 1024;
const DATA_CHANNEL_LOW_WATER_BYTES: u32 = 64 * 1024;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct IceServerConfig {
    pub urls: Vec<String>,
    pub username: Option<String>,
    pub credential: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnRefreshFailureDisposition {
    KeepDirectSession,
    ReconnectRelaySession,
}

/// Chooses recovery only when the application-level TURN refresh fails permanently.
pub fn turn_refresh_failure_disposition(selected_pair_uses_relay: Option<bool>) -> TurnRefreshFailureDisposition {
    match selected_pair_uses_relay {
        Some(false) => TurnRefreshFailureDisposition::KeepDirectSession,
        Some(true) | None => TurnRefreshFailureDisposition::ReconnectRelaySession,
    }
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
                "[rtc] relay candidate gathered monotonic_ms={} transport={} relay_transport={:?}",
                monotonic_ms(),
                candidate.protocol,
                candidate.relay_protocol,
            );
        } else {
            log::debug!(
                "[rtc] ICE candidate gathered monotonic_ms={} type={candidate_type} transport={}",
                monotonic_ms(),
                candidate.protocol
            );
        }
    }

    async fn on_ice_candidate_error(&self, event: RTCPeerConnectionIceErrorEvent) {
        log::warn!(
            "[rtc] ICE server candidate error monotonic_ms={} code={} detail=<redacted>",
            monotonic_ms(),
            event.error_code
        );
    }

    async fn on_ice_connection_state_change(&self, state: RTCIceConnectionState) {
        log::info!("[rtc] ICE connection state monotonic_ms={} state={state:?}", monotonic_ms());
    }

    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        log::debug!("[rtc] ICE gathering state monotonic_ms={} state={state}", monotonic_ms());
        if state == RTCIceGatheringState::Complete {
            if let Ok(counts) = self.candidate_counts.lock() {
                log::info!(
                    "[rtc] ICE gathering complete monotonic_ms={} candidates host={} srflx={} relay={} prflx={}",
                    monotonic_ms(),
                    counts.host,
                    counts.srflx,
                    counts.relay,
                    counts.prflx,
                );
            }
            self.gathering.send_replace(true);
        }
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        log::info!("[rtc] PeerConnection state monotonic_ms={} state={state:?}", monotonic_ms());
        self.connected.send_replace(state == RTCPeerConnectionState::Connected);
        self.failed
            .send_replace(matches!(state, RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed));
    }

    async fn on_data_channel(&self, channel: Arc<dyn DataChannel>) {
        let _ = self.channels.try_send(channel);
    }
}

/// A negotiated peer with bounded JSON framing over an ordered reliable channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerCloseReason {
    ManualDisconnect,
    RoomClosed,
    TurnRotation,
    ApiSessionEnded,
    LocalShutdown,
    DropCleanup,
}

/// Only a closed DataChannel with a recorded local lifecycle reason is an expected
/// receive-pump termination. Other errors remain visible as warnings.
pub fn expected_local_receive_close(error: &RtcError, reason: Option<PeerCloseReason>) -> bool {
    matches!(error, RtcError::Closed) && reason.is_some()
}

pub struct RtcPeer {
    connection: Arc<dyn PeerConnection>,
    gathering_rx: watch::Receiver<bool>,
    connected_rx: watch::Receiver<bool>,
    failed_rx: watch::Receiver<bool>,
    channel_rx: mpsc::Receiver<Arc<dyn DataChannel>>,
    channel: Arc<Mutex<Option<Arc<dyn DataChannel>>>>,
    channel_open: watch::Receiver<bool>,
    channel_open_sender: watch::Sender<bool>,
    channel_closed: watch::Receiver<bool>,
    channel_closed_sender: watch::Sender<bool>,
    close_reason: watch::Receiver<Option<PeerCloseReason>>,
    close_reason_sender: watch::Sender<Option<PeerCloseReason>>,
    buffered_high: watch::Receiver<bool>,
    buffered_high_sender: watch::Sender<bool>,
    channel_event_task: StdMutex<Option<tokio::task::JoinHandle<()>>>,
    candidate_counts: Arc<StdMutex<CandidateCounts>>,
    close_started: AtomicBool,
    close_completed: AtomicBool,
    inbound: Mutex<mpsc::Receiver<Vec<u8>>>,
    inbound_sender: mpsc::Sender<Vec<u8>>,
    decoder: Mutex<FrameDecoder>,
    ice_servers: Mutex<Vec<IceServerConfig>>,
    force_relay: bool,
}

impl RtcPeer {
    pub async fn new(
        ice_servers: &[IceServerConfig],
        udp_addrs: Vec<String>,
        force_relay: bool,
    ) -> Result<Self, RtcError> {
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
            .map_err(peer_error)?;
        let connection: Arc<dyn PeerConnection> = Arc::new(connection);
        log::info!("[rtc] PeerConnection created; force_relay={force_relay}; ICE servers={ice_server_count}");
        let (channel_open_tx, channel_open) = watch::channel(false);
        let (channel_closed_tx, channel_closed) = watch::channel(false);
        let (close_reason_tx, close_reason) = watch::channel(None);
        let (buffered_high_sender, buffered_high) = watch::channel(false);
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
            channel_closed,
            channel_closed_sender: channel_closed_tx,
            close_reason,
            close_reason_sender: close_reason_tx,
            buffered_high,
            buffered_high_sender,
            channel_event_task: StdMutex::new(None),
            candidate_counts,
            close_started: AtomicBool::new(false),
            close_completed: AtomicBool::new(false),
            inbound: Mutex::new(inbound),
            inbound_sender: inbound_tx,
            decoder: Mutex::new(FrameDecoder::default()),
            ice_servers: Mutex::new(ice_servers.to_vec()),
            force_relay,
        };
        Ok(peer)
    }

    /// Stages fresh TURN credentials in the PeerConnection configuration.
    ///
    /// The pinned WebRTC driver keeps the active gathering intact and defers the new
    /// configuration until a later ICE gathering. This does not rotate authentication on an
    /// existing TURN allocation: RFC 8656 binds that allocation to its original username.
    pub async fn update_turn_credentials(&self, server: IceServerConfig) -> Result<(), RtcError> {
        let mut current = self.ice_servers.lock().await;
        let next = replace_turn_credentials(&current, &server)?;
        let configuration = ice_configuration(&next, self.force_relay)?;
        self.connection
            .set_configuration(configuration)
            .await
            .map_err(peer_error)?;
        *current = next;
        log::info!(
            "[rtc] TURN credentials staged monotonic_ms={} active_allocation_reauthenticated=false",
            monotonic_ms()
        );
        Ok(())
    }

    pub async fn create_offer(&mut self) -> Result<String, RtcError> {
        if self.channel.lock().await.is_none() {
            let channel = self
                .connection
                .create_data_channel(
                    "shockingvrc-remote-v1",
                    Some(RTCDataChannelInit {
                        ordered: true,
                        max_packet_life_time: None,
                        max_retransmits: None,
                        ..Default::default()
                    }),
                )
                .await
                .map_err(peer_error)?;
            self.attach_channel(channel).await;
        }
        let offer = self.connection.create_offer(None).await.map_err(peer_error)?;
        self.connection.set_local_description(offer).await.map_err(peer_error)?;
        self.wait_gathering().await?;
        self.connection
            .local_description()
            .await
            .map(|description| description.sdp)
            .ok_or_else(|| RtcError::Negotiation("local SDP was not produced".into()))
    }

    pub async fn accept_offer(&mut self, offer_sdp: &str) -> Result<String, RtcError> {
        let offer = RTCSessionDescription::offer(offer_sdp.to_owned()).map_err(peer_error)?;
        self.connection
            .set_remote_description(offer)
            .await
            .map_err(peer_error)?;
        let answer = self.connection.create_answer(None).await.map_err(peer_error)?;
        self.connection
            .set_local_description(answer)
            .await
            .map_err(peer_error)?;
        self.wait_gathering().await?;
        self.connection
            .local_description()
            .await
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
        let mut failed = self.failed_rx.clone();
        let channel = tokio::time::timeout(DATA_CHANNEL_TIMEOUT, async {
            loop {
                if *failed.borrow() {
                    return Err(RtcError::Closed);
                }
                tokio::select! {
                    channel = self.channel_rx.recv() => return channel.ok_or(RtcError::Closed),
                    result = failed.changed() => {
                        result.map_err(|_| RtcError::Closed)?;
                        if *failed.borrow() { return Err(RtcError::Closed); }
                    }
                }
            }
        })
        .await
        .map_err(|_| RtcError::Timeout("waiting for data channel"))??;
        self.attach_channel(channel).await;
        Ok(())
    }

    pub async fn wait_open(&mut self) -> Result<(), RtcError> {
        self.wait_data_channel().await?;
        let mut open = self.channel_open.clone();
        let mut closed = self.channel_closed.clone();
        tokio::time::timeout(DATA_CHANNEL_TIMEOUT, async move {
            loop {
                if *open.borrow() {
                    return Ok(());
                }
                if *closed.borrow() {
                    return Err(RtcError::Closed);
                }
                tokio::select! {
                    result = open.changed() => result.map_err(|_| RtcError::Closed)?,
                    result = closed.changed() => {
                        result.map_err(|_| RtcError::Closed)?;
                        if *closed.borrow() { return Err(RtcError::Closed); }
                    }
                }
            }
        })
        .await
        .map_err(|_| RtcError::Timeout("opening data channel"))?
    }

    pub async fn wait_connected(&mut self) -> Result<(), RtcError> {
        let mut connected = self.connected_rx.clone();
        let mut failed = self.failed_rx.clone();
        tokio::time::timeout(DATA_CHANNEL_TIMEOUT, async move {
            loop {
                if *connected.borrow() {
                    return Ok(());
                }
                if *failed.borrow() {
                    return Err(RtcError::Closed);
                }
                tokio::select! {
                    result = connected.changed() => result.map_err(|_| RtcError::Closed)?,
                    result = failed.changed() => result.map_err(|_| RtcError::Closed)?,
                }
            }
        })
        .await
        .map_err(|_| RtcError::Timeout("connecting ICE/DTLS"))?
    }

    pub async fn fingerprints(&self) -> Result<DtlsFingerprints, RtcError> {
        let report = self.connection.get_stats(Instant::now(), StatsSelector::None).await;
        let mut local_id = None;
        let mut remote_id = None;
        for entry in report.iter() {
            if let RTCStatsReportEntry::Transport(transport) = entry {
                if !transport.local_certificate_id.is_empty() {
                    local_id = Some(transport.local_certificate_id.clone());
                }
                if !transport.remote_certificate_id.is_empty() {
                    remote_id = Some(transport.remote_certificate_id.clone());
                }
            }
        }
        let mut local = None;
        let mut remote = None;
        for entry in report.iter() {
            if let RTCStatsReportEntry::Certificate(certificate) = entry {
                if Some(certificate.stats.id.as_str()) == local_id.as_deref() {
                    local = Some(normalize_dtls_fingerprint(
                        &certificate.fingerprint,
                        &certificate.fingerprint_algorithm,
                    )?);
                }
                if Some(certificate.stats.id.as_str()) == remote_id.as_deref() {
                    remote = Some(normalize_dtls_fingerprint(
                        &certificate.fingerprint,
                        &certificate.fingerprint_algorithm,
                    )?);
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
            "[rtc] selected ICE pair monotonic_ms={} pair={label} source=live_ice_transport",
            monotonic_ms()
        );
        Some(label)
    }

    /// Returns whether the live selected pair uses a TURN relay candidate.
    /// `None` means WebRTC has not exposed a selected pair yet.
    pub async fn selected_pair_uses_relay(&self) -> Option<bool> {
        let sctp = self.connection.sctp().await?;
        let ice = sctp.transport().ice_transport();
        let pair = ice.get_selected_candidate_pair().await.ok().flatten()?;
        Some(pair.local().typ == RTCIceCandidateType::Relay || pair.remote().typ == RTCIceCandidateType::Relay)
    }

    /// Returns the number of local ICE candidates gathered so far, without exposing addresses.
    pub async fn local_candidate_count(&self) -> usize {
        self.candidate_counts
            .lock()
            .map(|counts| (counts.host + counts.srflx + counts.relay + counts.prflx) as usize)
            .unwrap_or_default()
    }

    pub async fn send_json(&self, bytes: &[u8]) -> Result<(), RtcError> {
        let chunks = encode_frame(bytes).map_err(|error| RtcError::Frame(error.to_string()))?;
        let channel = self.channel.lock().await.clone().ok_or(RtcError::Closed)?;
        for chunk in chunks {
            channel
                .send(BytesMut::from(chunk.as_slice()))
                .await
                .map_err(peer_error)?;
        }
        log::debug!("[rtc] DataChannel API tx monotonic_ms={} bytes={}", monotonic_ms(), bytes.len());
        Ok(())
    }

    pub async fn recv_json(&self) -> Result<Vec<u8>, RtcError> {
        let mut inbound = self.inbound.lock().await;
        let mut closed = self.channel_closed.clone();
        loop {
            if *closed.borrow() {
                return Err(RtcError::Closed);
            }
            let chunk = tokio::select! {
                chunk = inbound.recv() => chunk.ok_or(RtcError::Closed)?,
                result = closed.changed() => {
                    result.map_err(|_| RtcError::Closed)?;
                    if *closed.borrow() { return Err(RtcError::Closed); }
                    continue;
                }
            };
            if let Some(frame) = self
                .decoder
                .lock()
                .await
                .push(&chunk)
                .map_err(|error| RtcError::Frame(error.to_string()))?
            {
                log::debug!("[rtc] DataChannel API rx monotonic_ms={} bytes={}", monotonic_ms(), frame.len());
                return Ok(frame);
            }
        }
    }

    /// Bridges the RTC channel to bounded byte queues used by both the Host API and Terminal session loop.
    pub fn api_channels(self: &Arc<Self>) -> RtcApiChannels {
        let (inbound_tx, inbound) = mpsc::channel(32);
        let (control, mut control_rx) = mpsc::channel::<Vec<u8>>(32);
        let (replaceable, mut replaceable_rx) = watch::channel::<ReplaceableOutbox>(ReplaceableOutbox::default());
        let outbound = RtcApiOutbound {
            control: control.clone(),
            replaceable: replaceable.clone(),
            next_version: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            coalesced: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            buffered_high: self.buffered_high.clone(),
        };
        let metrics_control = control.clone();
        let metrics_replaceable = replaceable.clone();
        let metrics_coalesced = Arc::clone(&outbound.coalesced);
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
                        let reason = *receive_peer.close_reason.borrow();
                        if expected_local_receive_close(&error, reason) {
                            log::info!("[rtc] DataChannel API receive pump ended after local closure ({reason:?})");
                        } else {
                            log::warn!("[rtc] DataChannel API receive pump ended: {error}");
                        }
                        break;
                    }
                }
            }
        });
        let send_peer = Arc::clone(self);
        let send_task = tokio::spawn(async move {
            let mut buffered_high = send_peer.buffered_high.clone();
            loop {
                tokio::select! {
                    biased;
                    frame = control_rx.recv() => {
                        let Some(frame) = frame else { break; };
                        if let Err(error) = send_peer.send_json(&frame).await {
                            log::warn!("[rtc] DataChannel API control send pump ended: {error}");
                            break;
                        }
                    }
                    changed = replaceable_rx.changed(), if !*buffered_high.borrow() => {
                        if changed.is_err() { break; }
                        let Some(frame) = replaceable_rx.borrow_and_update().next_frame() else { continue; };
                        let age_ms = frame.queued_at.elapsed().as_millis();
                        if let Err(error) = send_peer.send_json(&frame.payload).await {
                            log::warn!("[rtc] DataChannel API replaceable state send pump ended: {error}");
                            break;
                        }
                        let control_depth = metrics_control.max_capacity().saturating_sub(metrics_control.capacity());
                        let replaceable_depth = metrics_replaceable.borrow().depth();
                        let coalesced = metrics_coalesced.load(Ordering::Relaxed);
                        log::debug!("[rtc] DataChannel latest-state frame sent monotonic_ms={} queue_age_ms={} payload_bytes={} control_queue_depth={} replaceable_queue_depth={} coalesced_state_frames={}", monotonic_ms(), age_ms, frame.payload.len(), control_depth, replaceable_depth, coalesced);
                        let version = frame.version;
                        let kind = frame.kind;
                        let _ = replaceable.send_if_modified(|current| {
                            current.clear_sent(kind, version)
                        });
                    }
                    changed = buffered_high.changed() => {
                        if changed.is_err() { break; }
                        if *buffered_high.borrow() {
                            log::info!("[rtc] DataChannel latest-state flow paused monotonic_ms={} reason=buffered_amount_high", monotonic_ms());
                        } else {
                            log::info!("[rtc] DataChannel latest-state flow resumed monotonic_ms={} reason=buffered_amount_low", monotonic_ms());
                        }
                    }
                }
            }
        });
        RtcApiChannels {
            inbound: Some(inbound),
            outbound: Some(outbound),
            tasks: vec![receive_task, send_task],
        }
    }

    pub async fn close(&self) {
        self.close_with_reason(PeerCloseReason::LocalShutdown).await;
    }

    pub async fn close_with_reason(&self, reason: PeerCloseReason) {
        if self.close_reason.borrow().is_none() {
            self.close_reason_sender.send_replace(Some(reason));
        }
        if self.close_completed.load(Ordering::Acquire) || self.close_started.swap(true, Ordering::AcqRel) {
            return;
        }
        log::info!(
            "[rtc] local peer close requested monotonic_ms={} reason={reason:?}",
            monotonic_ms()
        );
        self.channel_closed_sender.send_replace(true);
        if let Some(channel) = self.channel.lock().await.as_ref() {
            log::info!("[rtc] local DataChannel close requested monotonic_ms={}", monotonic_ms());
            let _ = tokio::time::timeout(Duration::from_secs(2), channel.close()).await;
        }
        let connection_closed = matches!(
            tokio::time::timeout(Duration::from_secs(3), self.connection.close()).await,
            Ok(Ok(())),
        );
        if connection_closed {
            self.close_completed.store(true, Ordering::Release);
        } else {
            log::warn!("[rtc] PeerConnection close did not complete; leaving a Drop fallback for cleanup");
        }
        self.close_started.store(false, Ordering::Release);
        let task = self.channel_event_task.lock().ok().and_then(|mut task| task.take());
        if let Some(mut task) = task {
            if tokio::time::timeout(Duration::from_secs(2), &mut task).await.is_err() {
                task.abort();
                let _ = task.await;
            }
        }
    }

    async fn wait_gathering(&mut self) -> Result<(), RtcError> {
        let mut gathering = self.gathering_rx.clone();
        tokio::time::timeout(ICE_GATHER_TIMEOUT, async move {
            loop {
                if *gathering.borrow() {
                    return Ok(());
                }
                gathering.changed().await.map_err(|_| RtcError::Closed)?;
            }
        })
        .await
        .map_err(|_| RtcError::Timeout("gathering ICE candidates"))?
    }

    async fn attach_channel(&mut self, channel: Arc<dyn DataChannel>) {
        log::info!("[rtc] DataChannel attached");
        if let Err(error) = channel.set_buffered_amount_high_threshold(DATA_CHANNEL_HIGH_WATER_BYTES).await {
            log::warn!("[rtc] could not set DataChannel high-water threshold: {error}");
        }
        if let Err(error) = channel.set_buffered_amount_low_threshold(DATA_CHANNEL_LOW_WATER_BYTES).await {
            log::warn!("[rtc] could not set DataChannel low-water threshold: {error}");
        }
        *self.channel.lock().await = Some(channel.clone());
        if let Some(previous) = self.channel_event_task.lock().ok().and_then(|mut task| task.take()) {
            previous.abort();
            let _ = previous.await;
        }
        let open_sender = self.channel_open_sender.clone();
        let closed_sender = self.channel_closed_sender.clone();
        let buffered_high_sender = self.buffered_high_sender.clone();
        let inbound_sender = self.inbound_sender.clone();
        let connection = Arc::clone(&self.connection);
        let task = tokio::spawn(async move {
            let mut stats_timer = tokio::time::interval(Duration::from_secs(15));
            stats_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut previous_pair: Option<String> = None;
            loop {
                tokio::select! {
                    event = channel.poll() => {
                        let Some(event) = event else { break; };
                        match event {
                            DataChannelEvent::OnOpen => {
                                log::info!("[rtc] DataChannel opened monotonic_ms={}", monotonic_ms());
                                open_sender.send_replace(true);
                            }
                            DataChannelEvent::OnMessage(message) => {
                                if inbound_sender.send(message.data.to_vec()).await.is_err() {
                                    break;
                                }
                            }
                            DataChannelEvent::OnError => {
                                log::warn!("[rtc] DataChannel error event monotonic_ms={} detail=<not exposed by WebRTC API>", monotonic_ms());
                            }
                            DataChannelEvent::OnBufferedAmountHigh => {
                                buffered_high_sender.send_replace(true);
                                log::info!("[rtc] DataChannel buffered amount crossed high-water threshold monotonic_ms={}", monotonic_ms());
                            }
                            DataChannelEvent::OnBufferedAmountLow => {
                                buffered_high_sender.send_replace(false);
                                log::info!("[rtc] DataChannel buffered amount crossed low-water threshold monotonic_ms={}", monotonic_ms());
                            }
                            DataChannelEvent::OnClosing => {
                                log::info!("[rtc] DataChannel closing event monotonic_ms={}", monotonic_ms());
                            }
                            DataChannelEvent::OnClose => {
                                log::info!(
                                    "[rtc] DataChannel close event monotonic_ms={}; remote close reason is not exposed by the WebRTC API",
                                    monotonic_ms()
                                );
                                open_sender.send_replace(false);
                                closed_sender.send_replace(true);
                                break;
                            }
                            _ => {}
                        }
                    }
                    _ = stats_timer.tick() => {
                        log_transport_stats(&connection, &channel, &mut previous_pair).await;
                    }
                }
            }
            log::debug!("[rtc] DataChannel event stream ended monotonic_ms={}", monotonic_ms());
            open_sender.send_replace(false);
            closed_sender.send_replace(true);
        });
        if let Ok(mut event_task) = self.channel_event_task.lock() {
            *event_task = Some(task);
        } else {
            task.abort();
        }
    }
}

async fn log_transport_stats(connection: &Arc<dyn PeerConnection>, data_channel: &Arc<dyn DataChannel>, previous_pair: &mut Option<String>) {
    let selected_pair = if let Some(sctp) = connection.sctp().await {
        let ice = sctp.transport().ice_transport();
        ice.get_selected_candidate_pair().await.ok().flatten().map(|pair| {
            candidate_pair_label(
                &pair.local().typ.to_string(),
                &pair.local().protocol.to_string(),
                &pair.remote().typ.to_string(),
                &pair.remote().protocol.to_string(),
                "succeeded",
            )
        })
    } else {
        None
    };
    if selected_pair != *previous_pair {
        log::info!(
            "[rtc] selected ICE pair transition monotonic_ms={} pair={}",
            monotonic_ms(),
            selected_pair.as_deref().unwrap_or("unavailable")
        );
        *previous_pair = selected_pair.clone();
    }

    let report = connection.get_stats(Instant::now(), StatsSelector::None).await;
    let outstanding_bytes = data_channel.outstanding_bytes().await.unwrap_or_default();
    let data_channel = report.data_channels().fold((0_u64, 0_u64, 0_u64, 0_u64), |sum, stats| {
        (
            sum.0 + u64::from(stats.messages_sent),
            sum.1 + u64::from(stats.messages_received),
            sum.2 + stats.bytes_sent,
            sum.3 + stats.bytes_received,
        )
    });
    let nominated_pair = report
        .candidate_pairs()
        .filter(|pair| pair.nominated)
        .fold((0_u64, 0_u64, 0_u32, 0_u64, 0_u64, 0.0_f64, 0_u64), |sum, pair| {
            (
                sum.0 + pair.bytes_sent,
                sum.1 + pair.bytes_received,
                sum.2 + 1,
                sum.3 + u64::from(pair.packets_sent),
                sum.4 + u64::from(pair.packets_received),
                sum.5.max(pair.current_round_trip_time * 1000.0),
                sum.6 + u64::from(pair.packets_discarded_on_send),
            )
        });
    log::debug!(
        "[rtc] transport statistics monotonic_ms={} selected_pair={} datachannel_messages_tx={} datachannel_messages_rx={} datachannel_bytes_tx={} datachannel_bytes_rx={} datachannel_outstanding_bytes={} nominated_pairs={} nominated_bytes_tx={} nominated_bytes_rx={} nominated_packets_tx={} nominated_packets_rx={} nominated_rtt_ms={:.2} nominated_packets_discarded_on_send={}",
        monotonic_ms(),
        selected_pair.as_deref().unwrap_or("unavailable"),
        data_channel.0,
        data_channel.1,
        data_channel.2,
        data_channel.3,
        outstanding_bytes,
        nominated_pair.2,
        nominated_pair.0,
        nominated_pair.1,
        nominated_pair.3,
        nominated_pair.4,
        nominated_pair.5,
        nominated_pair.6,
    );
}

impl Drop for RtcPeer {
    fn drop(&mut self) {
        if self.close_reason.borrow().is_none() {
            self.close_reason_sender
                .send_replace(Some(PeerCloseReason::DropCleanup));
        }
        self.channel_closed_sender.send_replace(true);
        if let Ok(event_task) = self.channel_event_task.get_mut()
            && let Some(task) = event_task.take()
        {
            task.abort();
        }
        if !self.close_completed.load(Ordering::Acquire) {
            let connection = Arc::clone(&self.connection);
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    let _ = tokio::time::timeout(Duration::from_secs(3), connection.close()).await;
                });
            }
        }
    }
}

fn ice_transport_policy(force_relay: bool) -> RTCIceTransportPolicy {
    if force_relay {
        RTCIceTransportPolicy::Relay
    } else {
        RTCIceTransportPolicy::All
    }
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

    log::info!(
        "[rtc] selected {} OS-routed local UDP address(es) for TURN gathering",
        local_ips.len()
    );
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
    if force_relay
        && !servers.iter().any(|server| {
            let has_turn_uri = server.urls.iter().any(|url| {
                let url = url.to_ascii_lowercase();
                url.starts_with("turn:") || url.starts_with("turns:")
            });
            has_turn_uri
                && server.username.as_deref().is_some_and(|value| !value.is_empty())
                && server.credential.as_deref().is_some_and(|value| !value.is_empty())
        })
    {
        return Err(RtcError::Configuration(
            "relay-only mode requires an authenticated TURN server with credentials".into(),
        ));
    }
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

fn replace_turn_credentials(
    current: &[IceServerConfig],
    replacement: &IceServerConfig,
) -> Result<Vec<IceServerConfig>, RtcError> {
    let invalid = || RtcError::Configuration("replacement TURN credentials or server URIs are invalid".into());
    if replacement.urls.is_empty()
        || replacement.username.as_deref().is_none_or(str::is_empty)
        || replacement.credential.as_deref().is_none_or(str::is_empty)
    {
        return Err(RtcError::Configuration("replacement TURN credentials are incomplete".into()));
    }

    let mut replacement_uris = Vec::with_capacity(replacement.urls.len());
    let mut replacement_set = HashSet::with_capacity(replacement.urls.len());
    for uri in &replacement.urls {
        let canonical = normalize_ice_server_uri(uri).map_err(|_| invalid())?;
        if !canonical.starts_with("turn:") && !canonical.starts_with("turns:") {
            return Err(invalid());
        }
        if replacement_set.insert(canonical.clone()) {
            replacement_uris.push(canonical);
        }
    }

    let mut match_counts = vec![0usize; replacement_uris.len()];
    let mut next = Vec::with_capacity(current.len() + replacement_uris.len());
    for active in current {
        let mut matching_urls = Vec::new();
        let mut unrelated_urls = Vec::new();
        for uri in &active.urls {
            let target_index = normalize_ice_server_uri(uri)
                .ok()
                .and_then(|canonical| replacement_uris.iter().position(|target| target == &canonical));
            if let Some(index) = target_index {
                match_counts[index] += 1;
                matching_urls.push(uri.clone());
            } else {
                unrelated_urls.push(uri.clone());
            }
        }

        if matching_urls.is_empty() {
            next.push(active.clone());
            continue;
        }
        if !unrelated_urls.is_empty() {
            next.push(IceServerConfig {
                urls: unrelated_urls,
                username: active.username.clone(),
                credential: active.credential.clone(),
            });
        }
        next.push(IceServerConfig {
            urls: matching_urls,
            username: replacement.username.clone(),
            credential: replacement.credential.clone(),
        });
    }

    if match_counts.iter().any(|count| *count != 1) {
        return Err(RtcError::Configuration(
            "replacement TURN server URLs do not uniquely match the active ICE configuration".into(),
        ));
    }
    Ok(next)
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
        IceServerConfig, PeerCloseReason, RtcError, RtcPeer, TurnRefreshFailureDisposition, candidate_pair_label,
        expected_local_receive_close, ice_configuration, ice_transport_policy, parse_udp_turn_endpoint,
        replace_turn_credentials, route_selected_udp_addrs, turn_refresh_failure_disposition,
    };
    use std::time::Duration;
    use webrtc::peer_connection::RTCIceTransportPolicy;

    #[test]
    fn force_relay_selects_relay_only_policy_and_default_keeps_all_candidates() {
        assert_eq!(ice_transport_policy(true), RTCIceTransportPolicy::Relay);
        assert_eq!(ice_transport_policy(false), RTCIceTransportPolicy::All);
    }

    #[test]
    fn force_relay_rejects_configuration_without_turn() {
        let servers = vec![IceServerConfig {
            urls: vec!["stun:stun.example.test:3478".to_owned()],
            username: None,
            credential: None,
        }];
        assert!(matches!(ice_configuration(&servers, true), Err(RtcError::Configuration(_))));
    }

    #[test]
    fn force_relay_accepts_authenticated_turn_configuration() {
        let servers = vec![IceServerConfig {
            urls: vec!["turn:turn.example.test:3478?transport=udp".to_owned()],
            username: Some("ephemeral-user".to_owned()),
            credential: Some("ephemeral-credential".to_owned()),
        }];
        assert!(ice_configuration(&servers, true).is_ok());
    }

    #[test]
    fn permanent_turn_refresh_failure_keeps_direct_but_reconnects_relay_or_unknown_paths() {
        assert_eq!(
            turn_refresh_failure_disposition(Some(false)),
            TurnRefreshFailureDisposition::KeepDirectSession
        );
        assert_eq!(
            turn_refresh_failure_disposition(Some(true)),
            TurnRefreshFailureDisposition::ReconnectRelaySession
        );
        assert_eq!(
            turn_refresh_failure_disposition(None),
            TurnRefreshFailureDisposition::ReconnectRelaySession
        );
    }

    #[test]
    fn receive_pump_only_classifies_closed_after_a_known_local_reason_as_expected() {
        assert!(expected_local_receive_close(
            &RtcError::Closed,
            Some(PeerCloseReason::ManualDisconnect),
        ));
        assert!(expected_local_receive_close(
            &RtcError::Closed,
            Some(PeerCloseReason::TurnRotation),
        ));
        assert!(!expected_local_receive_close(&RtcError::Closed, None));
        assert!(!expected_local_receive_close(
            &RtcError::WebRtc("socket reset".to_owned()),
            Some(PeerCloseReason::ManualDisconnect),
        ));
    }

    #[test]
    fn turn_rotation_updates_matching_urls_independent_of_order_and_preserves_other_servers() {
        let active = vec![
            IceServerConfig {
                urls: vec!["stun:stun.example.test:3478".to_owned()],
                username: None,
                credential: None,
            },
            IceServerConfig {
                urls: vec!["turn:relay-a.example.test:3478?transport=udp".to_owned()],
                username: Some("old-user".to_owned()),
                credential: Some("old-password".to_owned()),
            },
            IceServerConfig {
                urls: vec!["turn:relay-b.example.test:3478?transport=udp".to_owned()],
                username: Some("old-user".to_owned()),
                credential: Some("old-password".to_owned()),
            },
            IceServerConfig {
                urls: vec!["turn:custom.example.test:3478?transport=udp".to_owned()],
                username: Some("custom-user".to_owned()),
                credential: Some("custom-password".to_owned()),
            },
        ];
        let replacement = IceServerConfig {
            urls: vec![
                "turn:relay-b.example.test:3478?transport=udp".to_owned(),
                "turn:relay-a.example.test:3478?transport=udp".to_owned(),
            ],
            username: Some("new-user".to_owned()),
            credential: Some("new-password".to_owned()),
        };

        let updated = replace_turn_credentials(&active, &replacement).expect("matching endpoint set");

        assert_eq!(updated.len(), 4);
        assert_eq!(updated[0].username, None);
        assert_eq!(updated[1].username.as_deref(), Some("new-user"));
        assert_eq!(updated[1].credential.as_deref(), Some("new-password"));
        assert_eq!(updated[2].username.as_deref(), Some("new-user"));
        assert_eq!(updated[2].credential.as_deref(), Some("new-password"));
        assert_eq!(updated[3].username.as_deref(), Some("custom-user"));
        assert_eq!(updated[3].credential.as_deref(), Some("custom-password"));
    }

    #[test]
    fn turn_rotation_matches_equivalent_uri_representations() {
        let active = vec![IceServerConfig {
            urls: vec!["TURN:Relay.Example.Test?transport=UDP".to_owned()],
            username: Some("old-user".to_owned()),
            credential: Some("old-password".to_owned()),
        }];
        let replacement = IceServerConfig {
            urls: vec!["turn:relay.example.test:3478?transport=udp".to_owned()],
            username: Some("new-user".to_owned()),
            credential: Some("new-password".to_owned()),
        };

        let updated = replace_turn_credentials(&active, &replacement).expect("canonical URI match");

        assert_eq!(updated.len(), 1);
        assert_eq!(updated[0].username.as_deref(), Some("new-user"));
    }

    #[test]
    fn turn_rotation_fails_closed_for_changed_or_unmatched_endpoints() {
        let active = vec![IceServerConfig {
            urls: vec!["turn:active.example.test:3478?transport=udp".to_owned()],
            username: Some("old-user".to_owned()),
            credential: Some("old-password".to_owned()),
        }];
        let replacement = IceServerConfig {
            urls: vec!["turn:changed.example.test:3478?transport=udp".to_owned()],
            username: Some("new-user".to_owned()),
            credential: Some("new-password".to_owned()),
        };

        assert!(matches!(
            replace_turn_credentials(&active, &replacement),
            Err(RtcError::Configuration(_))
        ));
        assert_eq!(active[0].username.as_deref(), Some("old-user"));
    }

    #[test]
    fn turn_rotation_rejects_missing_credentials_and_non_turn_uris() {
        let active = vec![IceServerConfig {
            urls: vec!["turn:relay.example.test:3478?transport=udp".to_owned()],
            username: Some("old-user".to_owned()),
            credential: Some("old-password".to_owned()),
        }];
        let no_password = IceServerConfig {
            urls: active[0].urls.clone(),
            username: Some("new-user".to_owned()),
            credential: Some(String::new()),
        };
        let stun_url = IceServerConfig {
            urls: vec!["stun:stun.example.test:3478".to_owned()],
            username: Some("new-user".to_owned()),
            credential: Some("new-password".to_owned()),
        };

        assert!(matches!(
            replace_turn_credentials(&active, &no_password),
            Err(RtcError::Configuration(_))
        ));
        assert!(matches!(
            replace_turn_credentials(&active, &stun_url),
            Err(RtcError::Configuration(_))
        ));
    }

    #[test]
    fn turn_rotation_splits_a_mixed_server_entry_without_replacing_unrelated_urls() {
        let active = vec![IceServerConfig {
            urls: vec![
                "turn:relay.example.test:3478?transport=udp".to_owned(),
                "stun:custom.example.test:3478".to_owned(),
            ],
            username: Some("custom-user".to_owned()),
            credential: Some("custom-password".to_owned()),
        }];
        let replacement = IceServerConfig {
            urls: vec!["turn:relay.example.test:3478?transport=udp".to_owned()],
            username: Some("new-user".to_owned()),
            credential: Some("new-password".to_owned()),
        };

        let updated = replace_turn_credentials(&active, &replacement).expect("TURN URI match");

        assert_eq!(updated.len(), 2);
        assert_eq!(updated[0].urls, ["stun:custom.example.test:3478"]);
        assert_eq!(updated[0].username.as_deref(), Some("custom-user"));
        assert_eq!(updated[1].urls, ["turn:relay.example.test:3478?transport=udp"]);
        assert_eq!(updated[1].username.as_deref(), Some("new-user"));
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
        assert_eq!(parse_udp_turn_endpoint("turn:turn.example.test?transport=tcp"), None,);
    }

    #[tokio::test]
    async fn route_selection_preserves_explicit_and_direct_only_bind_addresses() {
        let configured = vec!["0.0.0.0:0".to_owned()];
        assert_eq!(route_selected_udp_addrs(&[], configured.clone()).await, configured,);

        let configured = vec!["127.0.0.1:0".to_owned()];
        let servers = vec![IceServerConfig {
            urls: vec!["turn:turn.example.test:3478?transport=udp".to_owned()],
            username: None,
            credential: None,
        }];
        assert_eq!(route_selected_udp_addrs(&servers, configured.clone()).await, configured,);
    }

    #[tokio::test]
    async fn connected_data_channel_exposes_selected_candidate_pair() {
        let bind = vec!["127.0.0.1:0".to_owned()];
        let mut terminal = RtcPeer::new(&[], bind.clone(), false)
            .await
            .expect("Terminal peer should build");
        let mut host = RtcPeer::new(&[], bind, false).await.expect("Host peer should build");

        let offer = terminal.create_offer().await.expect("Terminal offer");
        let answer = host.accept_offer(&offer).await.expect("Host answer");
        terminal.set_answer(&answer).await.expect("Terminal applies answer");

        tokio::try_join!(terminal.wait_connected(), host.wait_connected())
            .expect("ICE and DTLS should connect over loopback");
        tokio::try_join!(terminal.wait_open(), host.wait_open()).expect("DataChannel should open over loopback");

        assert_eq!(
            terminal.selected_candidate_pair_label().await.as_deref(),
            Some("LOCAL HOST/UDP ↔ REMOTE HOST/UDP · SUCCEEDED"),
        );
        assert_eq!(terminal.selected_pair_uses_relay().await, Some(false));
        assert_eq!(host.selected_pair_uses_relay().await, Some(false));
        terminal.close().await;
        host.close().await;
    }

    #[tokio::test]
    async fn remote_data_channel_close_ends_api_receive_stream() {
        use std::sync::Arc;

        let bind = vec!["127.0.0.1:0".to_owned()];
        let mut terminal = RtcPeer::new(&[], bind.clone(), false)
            .await
            .expect("Terminal peer should build");
        let mut host = RtcPeer::new(&[], bind, false).await.expect("Host peer should build");

        let offer = terminal.create_offer().await.expect("Terminal offer");
        let answer = host.accept_offer(&offer).await.expect("Host answer");
        terminal.set_answer(&answer).await.expect("Terminal applies answer");
        tokio::try_join!(terminal.wait_connected(), host.wait_connected())
            .expect("ICE and DTLS should connect over loopback");
        tokio::try_join!(terminal.wait_open(), host.wait_open()).expect("DataChannel should open over loopback");

        let terminal = Arc::new(terminal);
        let host = Arc::new(host);
        let mut host_channels = host.api_channels();
        let (mut inbound, _outbound) = host_channels.take().expect("Host API channels");

        terminal.close().await;
        let closed = tokio::time::timeout(Duration::from_secs(2), inbound.recv())
            .await
            .expect("remote DataChannel close should end the API receive stream");
        assert!(
            closed.is_none(),
            "remote DataChannel close should not leave API receive blocked"
        );

        host_channels.close(&host).await;
    }

    #[tokio::test]
    async fn control_frames_precede_coalesced_state_while_datachannel_is_congested() {
        use std::sync::Arc;

        let bind = vec!["127.0.0.1:0".to_owned()];
        let mut terminal = RtcPeer::new(&[], bind.clone(), false).await.expect("Terminal peer");
        let mut host = RtcPeer::new(&[], bind, false).await.expect("Host peer");
        let offer = terminal.create_offer().await.expect("Terminal offer");
        let answer = host.accept_offer(&offer).await.expect("Host answer");
        terminal.set_answer(&answer).await.expect("apply answer");
        tokio::try_join!(terminal.wait_connected(), host.wait_connected()).expect("loopback ICE");
        tokio::try_join!(terminal.wait_open(), host.wait_open()).expect("loopback DataChannel");

        host.buffered_high_sender.send_replace(true);
        let terminal = Arc::new(terminal);
        let host = Arc::new(host);
        let mut channels = host.api_channels();
        let (_inbound, outbound) = channels.take_optimized().expect("Host API channels");
        assert!(!outbound.replace_snapshot(br#"{"type":"snapshot","seq":1}"#.to_vec()).unwrap());
        for seq in 2..=101 {
            assert_eq!(outbound
                .replace_telemetry(serde_json::to_vec(&serde_json::json!({"type":"telemetry.update","seq":seq})).unwrap())
                .unwrap(), seq > 2);
        }
        assert_eq!(outbound.coalesced_state_count(), 99);
        assert_eq!(outbound.replaceable_queue_depth(), 2, "full state and the newest telemetry keyframe must remain bounded and independently pending");
        assert_eq!(outbound.control_queue_depth(), 0);
        outbound.send_control(br#"{"type":"pong","request_id":"control"}"#.to_vec()).await.unwrap();

        let control = tokio::time::timeout(Duration::from_secs(2), terminal.recv_json())
            .await.expect("control frame not blocked by state congestion").expect("control frame");
        assert_eq!(serde_json::from_slice::<serde_json::Value>(&control).unwrap()["type"], "pong");
        assert!(tokio::time::timeout(Duration::from_millis(50), terminal.recv_json()).await.is_err(), "replaceable state remains paused above high water");

        host.buffered_high_sender.send_replace(false);
        let full = tokio::time::timeout(Duration::from_secs(2), terminal.recv_json())
            .await.expect("authoritative keyframe was not released at low water").expect("full snapshot frame");
        let full: serde_json::Value = serde_json::from_slice(&full).unwrap();
        assert_eq!(full["type"], "snapshot");
        assert_eq!(full["seq"], 1);

        let latest = tokio::time::timeout(Duration::from_secs(2), terminal.recv_json())
            .await.expect("latest telemetry was not released after the full keyframe").expect("latest telemetry frame");
        let latest: serde_json::Value = serde_json::from_slice(&latest).unwrap();
        assert_eq!(latest["type"], "telemetry.update");
        assert_eq!(latest["seq"], 101);

        terminal.close().await;
        channels.close(&host).await;
    }
}

pub struct RtcApiChannels {
    inbound: Option<mpsc::Receiver<Vec<u8>>>,
    outbound: Option<RtcApiOutbound>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

#[derive(Clone)]
struct ReplaceableFrame {
    version: u64,
    payload: Vec<u8>,
    queued_at: Instant,
    kind: ReplaceableFrameKind,
}

#[derive(Clone, Copy)]
enum ReplaceableFrameKind {
    Snapshot,
    Telemetry,
}

#[derive(Clone, Default)]
struct ReplaceableOutbox {
    snapshot: Option<ReplaceableFrame>,
    telemetry: Option<ReplaceableFrame>,
}

impl ReplaceableOutbox {
    fn next_frame(&self) -> Option<ReplaceableFrame> {
        self.snapshot.clone().or_else(|| self.telemetry.clone())
    }

    fn depth(&self) -> usize {
        usize::from(self.snapshot.is_some()) + usize::from(self.telemetry.is_some())
    }

    fn clear_sent(&mut self, kind: ReplaceableFrameKind, version: u64) -> bool {
        let slot = match kind {
            ReplaceableFrameKind::Snapshot => &mut self.snapshot,
            ReplaceableFrameKind::Telemetry => &mut self.telemetry,
        };
        if slot.as_ref().is_some_and(|pending| pending.version == version) {
            *slot = None;
            true
        } else {
            false
        }
    }
}

/// Bounded API output: control is FIFO, full snapshots retain their own slot, and the
/// newest telemetry keyframe replaces only older telemetry that has not entered the channel.
#[derive(Clone)]
pub struct RtcApiOutbound {
    control: mpsc::Sender<Vec<u8>>,
    replaceable: watch::Sender<ReplaceableOutbox>,
    next_version: Arc<AtomicU64>,
    coalesced: Arc<AtomicU64>,
    buffered_high: watch::Receiver<bool>,
}

impl RtcApiOutbound {
    pub async fn send_control(&self, payload: Vec<u8>) -> Result<(), ()> {
        self.control.send(payload).await.map_err(|_| ())
    }

    pub fn replace_snapshot(&self, payload: Vec<u8>) -> Result<bool, ()> {
        self.replace(ReplaceableFrameKind::Snapshot, payload)
    }

    pub fn replace_telemetry(&self, payload: Vec<u8>) -> Result<bool, ()> {
        self.replace(ReplaceableFrameKind::Telemetry, payload)
    }

    fn replace(&self, kind: ReplaceableFrameKind, payload: Vec<u8>) -> Result<bool, ()> {
        if self.replaceable.is_closed() {
            return Err(());
        }
        let frame = ReplaceableFrame {
            version: self.next_version.fetch_add(1, Ordering::Relaxed),
            payload,
            queued_at: Instant::now(),
            kind,
        };
        let mut replaced = false;
        let changed = self.replaceable.send_if_modified(|outbox| {
            let slot = match kind {
                ReplaceableFrameKind::Snapshot => &mut outbox.snapshot,
                ReplaceableFrameKind::Telemetry => &mut outbox.telemetry,
            };
            replaced = slot.replace(frame).is_some();
            true
        });
        if !changed {
            return Err(());
        }
        if replaced {
            self.coalesced.fetch_add(1, Ordering::Relaxed);
        }
        Ok(replaced)
    }

    pub fn control_queue_depth(&self) -> usize {
        self.control.max_capacity().saturating_sub(self.control.capacity())
    }

    pub fn replaceable_queue_depth(&self) -> usize {
        self.replaceable.borrow().depth()
    }

    pub fn coalesced_state_count(&self) -> u64 {
        self.coalesced.load(Ordering::Relaxed)
    }

    pub fn is_congested(&self) -> bool {
        *self.buffered_high.borrow()
    }
}

impl RtcApiChannels {
    /// Legacy reliable FIFO output for peers that do not publish replaceable state.
    pub fn take(&mut self) -> Result<(mpsc::Receiver<Vec<u8>>, mpsc::Sender<Vec<u8>>), RtcError> {
        let inbound = self.inbound.take().ok_or(RtcError::Closed)?;
        let outbound = self.outbound.as_ref().ok_or(RtcError::Closed)?.control.clone();
        Ok((inbound, outbound))
    }

    /// Output with bounded reliable control plus a coalescing replaceable state slot.
    pub fn take_optimized(&mut self) -> Result<(mpsc::Receiver<Vec<u8>>, RtcApiOutbound), RtcError> {
        let inbound = self.inbound.take().ok_or(RtcError::Closed)?;
        let outbound = self.outbound.as_ref().ok_or(RtcError::Closed)?.clone();
        Ok((inbound, outbound))
    }

    pub async fn close(self, peer: &RtcPeer) {
        self.close_with_reason(peer, PeerCloseReason::ApiSessionEnded).await;
    }

    pub async fn close_with_reason(mut self, peer: &RtcPeer, reason: PeerCloseReason) {
        self.inbound.take();
        self.outbound.take();
        peer.close_with_reason(reason).await;
        for task in self.tasks.drain(..) {
            task.abort();
            let _ = task.await;
        }
    }
}

impl Drop for RtcApiChannels {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DtlsFingerprints {
    pub local: [u8; 32],
    pub remote: [u8; 32],
}

fn normalize_dtls_fingerprint(value: &str, algorithm: &str) -> Result<[u8; 32], RtcError> {
    if !algorithm.eq_ignore_ascii_case("sha-256") {
        return Err(RtcError::FingerprintAlgorithm);
    }
    let compact = value.replace(':', "");
    if compact.len() != 64 {
        return Err(RtcError::FingerprintUnavailable);
    }
    let mut bytes = [0_u8; 32];
    for (index, output) in bytes.iter_mut().enumerate() {
        let pair = compact
            .get(index * 2..index * 2 + 2)
            .ok_or(RtcError::FingerprintUnavailable)?;
        *output = u8::from_str_radix(pair, 16).map_err(|_| RtcError::FingerprintUnavailable)?;
    }
    Ok(bytes)
}

fn peer_error(error: impl std::fmt::Display) -> RtcError {
    RtcError::WebRtc(error.to_string())
}

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
