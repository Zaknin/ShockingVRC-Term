//! Reliable, ordered WebRTC DataChannel transport.

use std::{sync::Arc, time::{Duration, Instant}};

use bytes::BytesMut;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, mpsc, watch};
use webrtc::{
    data_channel::{DataChannel, DataChannelEvent, RTCDataChannelInit},
    peer_connection::{
        MediaEngine, PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler,
        RTCConfigurationBuilder, RTCIceGatheringState, RTCIceServer, RTCStatsReportEntry,
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
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for PeerEvents {
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            self.gathering.send_replace(true);
        }
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
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
}

impl RtcPeer {
    pub async fn new(ice_servers: &[IceServerConfig], udp_addrs: Vec<String>) -> Result<Self, RtcError> {
        let mut rtc_ice_servers = Vec::with_capacity(ice_servers.len());
        for server in ice_servers {
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

        let configuration = RTCConfigurationBuilder::new()
            .with_ice_servers(rtc_ice_servers)
            .build();
        let (gathering_tx, gathering_rx) = watch::channel(false);
        let (connected_tx, connected_rx) = watch::channel(false);
        let (failed_tx, failed_rx) = watch::channel(false);
        let (channel_tx, channel_rx) = mpsc::channel(2);
        let events = PeerEvents {
            gathering: gathering_tx,
            connected: connected_tx,
            failed: failed_tx,
            channels: channel_tx,
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
        };
        Ok(peer)
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
            while let Ok(frame) = receive_peer.recv_json().await {
                if inbound_tx.send(frame).await.is_err() { break; }
            }
        });
        let send_peer = Arc::clone(self);
        let send_task = tokio::spawn(async move {
            while let Some(frame) = outbound_rx.recv().await {
                if send_peer.send_json(&frame).await.is_err() { break; }
            }
        });
        RtcApiChannels { inbound: Some(inbound), outbound, tasks: vec![receive_task, send_task] }
    }

    pub async fn close(&self) {
        if let Some(channel) = self.channel.lock().await.as_ref() {
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
        *self.channel.lock().await = Some(channel.clone());
        let open_sender = self.channel_open_sender.clone();
        let inbound_sender = self.inbound_sender.clone();
        tokio::spawn(async move {
            while let Some(event) = channel.poll().await {
                match event {
                    DataChannelEvent::OnOpen => { open_sender.send_replace(true); }
                    DataChannelEvent::OnMessage(message) => {
                        if inbound_sender.send(message.data.to_vec()).await.is_err() { break; }
                    }
                    DataChannelEvent::OnClose => {
                        open_sender.send_replace(false);
                        break;
                    }
                    _ => {}
                }
            }
            open_sender.send_replace(false);
        });
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
