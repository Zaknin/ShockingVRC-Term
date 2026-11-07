//! Shared WebRTC DataChannel and rendezvous transport primitives.

pub mod binding;
pub mod frame;
pub mod peer;
pub mod session;
pub mod signaling;

pub use binding::{
    BindingError, BindingFields, IdentityProof, binding_transcript, identity_fingerprint, server_id, verify_binding,
};
pub use frame::{FrameDecoder, FrameError, encode_frame};
pub use peer::{DtlsFingerprints, IceServerConfig, RtcApiChannels, RtcError, RtcPeer};
pub use session::{
    EstablishedRtcSession, HostIdentitySigner, HostSignaling, RegisteredHost, SessionError, SignalingLease,
    TerminalRtcConfig, TurnCredentials, accept_host_room, close_host_room, ephemeral_identity, join_terminal_room,
    register_host, request_turn_credentials,
};
pub use signaling::{SignalError, SignalSocket, join_message, register_message, validate_signal_url};
