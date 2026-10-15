# 33. Host identity: ADR-033 feasibility and integration plan

**Status: CONDITIONALLY APPROVED DESIGN, NOT VERIFIED.** Owner approved ADR-033 in v0.15 contingent on library/crypto feasibility. This is an implementation-proof plan only, **not an audit**. Do not weaken identity verification or silently switch to trust-all TLS.

## Target trust model

Host has long-lived Ed25519 signing identity (private part stays on Host); Terminal explicitly pairs with its public identity fingerprint by reading it from Host's local egui screen, preferably QR or confirmed fingerprint. The saved pin identifies the Host across DDNS/IP and across either transport. Pairing is a user action; network hostname and room locator are insufficient.

### Direct WSS

- Windows native Rust networking calls `rustls`/configured verifier; use TLS server certificate/SPKI association with paired Host identity.
- Need to demonstrate exact binding: either X.509 SPKI can represent Host key and accepted algorithms in selected backend, or certified linkage from long-lived identity to a compatible TLS leaf key. A bare equality of unrelated Ed25519 fingerprint and TLS SPKI would be **incorrect**.
- Certificate renewal must preserve verifiable pinned identity. If the Host identity key changes, show re-pairing; do not auto-accept a replacement key.
- Ensure certificate SAN rules, TLS stack version and local-IP/DDNS use are handled by the chosen verifier without an insecure global certificate exception.

### RTC / DTLS

- Signed proof binds: protocol/version and fixed context, session nonces, server epoch, exact SDP DTLS certificate fingerprint(s), negotiated ephemeral identifiers, remote Host ID and handshake lifetime.
- Canonical byte encoding, replay prevention, selected candidate/role context and order must be specified before implementing signature checks. Never sign arbitrary JSON text without canonical encoding.
- WebRTC library must enforce actual DTLS fingerprint against negotiated SDP **and** expose enough certificate fingerprint data for the Terminal to validate signed Host-identity proof. No `disable_certificate_fingerprint_verification` toggles.
- TURN/rendezvous may read signaling metadata and can be replaced; neither must be trusted with Host credentials or trusted to claim Host identity.

## Minimal isolated prototype gates

| Gate | Evidence needed | Pass condition |
|---|---|---|
| C1 | Native Windows Rust TLS example | Self-hosted direct cert validated against pinned long-term identity, wrong cert rejected |
| C2 | Rust WebRTC/DTLS two-process example | Actual DTLS fingerprint equals approved signed proof; altered SDP/fingerprint refused |
| C3 | Identity rotation example | Intentional new Host identity requires explicit re-pair |
| C4 | Mixed paths | Same pin for direct WSS and RTC when changing endpoint/IP/hostname |
| C5 | Credential ordering | Login never sent before trust verification succeeds |

A failure in C1/C2 is a **design blocker for ADR-033**, not a reason to implement trust-all transport. Owner must approve a revised solution before credentialed remote sessions use it.

## Suggested candidate implementations (NOT preapproved dependencies)

- Rust `rustls` with customized verifier and appropriately generated X.509 chain; prove lifecycle and SPKI binding.
- `webrtc-rs` `PeerConnection` DataChannel support; inspect exact version-specific API for DTLS fingerprint/remote SDP and security settings.
- Native Terminal verification occurs in Rust, not WebView JavaScript.

Reference: https://github.com/webrtc-rs/webrtc ; https://docs.rs/rtc/latest/rtc/peer_connection/transport/struct.RTCDtlsFingerprint.html ; https://docs.rs/webrtc/latest/webrtc/data_channel/ . Library documentation is not proof that C1–C5 have been executed.

## Test-only keys and logs

Use generated disposable keys, isolated localhost sockets, and a fixture signaling server. No real Host password or hardware/device command is required. Record crate versions, input hashes, test transcript IDs and intended observations without saving private keys in evidence bundles. The audit methodology will be supplied later by the owner.
