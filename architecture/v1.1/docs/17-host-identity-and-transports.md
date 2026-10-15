# 17. Host identity and self-hosted transport trust — conditionally approved design

**CONDITIONALLY ACCEPTED (ADR-033), 2026-10-15; focused feasibility and security review REQUIRED before implementation of remote actuator/credentials paths.** The accepted requirements are TLS pinning without domain, verified Host before credentials, and independent self-hosted ICE/TURN infrastructure. The cryptographic construction is a normative **target** but not implemented, independently audited or proven feasible in the selected Rust/Tauri/WebRTC libraries. No trust-all fallback.

## Suggested long-term identity

- One Host install identity: randomly generated **Ed25519 keypair** stored on Host with OS-user protection; stable key persists across restart and optional server certificate renewal. `server_id=base32(SHA256(Ed25519 public key bytes))` is display identifier, fingerprint `SHA256(pubkey)` shown locally in Host GUI as readable hex/grouped representation/QR. User must verify first trust out-of-band with the Host before Terminal stores pin. No silent TOFU when first used remotely.
- Direct HTTPS: self-signed certificate generated around same long-term Host key, with SPKI bound to Ed25519 identity and appropriate SAN for configured IP/DDNS. Tauri Rust TLS verifier checks pinned **SPKI/public key**, hostname is routing metadata for self-signed pin mode, not independent authority. Do not disable TLS validation globally; do not blindly `accept_invalid_certs` without strict pin match. When Host key changes, require re-pairing, even if DDNS remains the same. CA-trusted ACME can later be supported as alternative, preserving pinned-identity behavior.
- RTC/DataChannel: ephemeral WebRTC DTLS fingerprint is advertised through **untrusted** signaling. Require a signed Host proof using long-term key over a canonical transcript (`protocol_version`, `server_epoch`, `client_nonce`, `server_nonce`, `transport_type`, DTLS certificate fingerprint, `session_id` if resuming) and validate signature + actually-negotiated remote peer fingerprint **before** sending password/ticket. Don't trust STUN, TURN or signaling to verify endpoint identity; the data channel must be DTLS-encrypted and properly authenticated.
- A compromised signaling server can cause denial of service or redirect traffic but must not impersonate a pinned Host. A compromised TURN sees encrypted DTLS application packets, not session bearer material in plaintext. Avoid putting Host password/tickets in SDP/candidates/signaling.

## BYO infrastructure

Default installed settings: `signaling_url: null`, `stun_servers: []`, `turn_servers: []`, `turn_credentials: null`; **no calls** to public STUN as hidden fallbacks. Direct LAN/WAN/VPN works without these services. Users can configure endpoints, secret references and obtain TURN credentials from their infrastructure; do not store long-lived TURN admin secrets in Terminal. Moscow server cohosting OpenShock is **private deployment example only**, not shipped defaults.

## Operational limitations

- ICE and TURN deal with UDP candidate paths and relay; STUN **doesn't open the router's TCP port**. WSS can use automatic UPnP/NAT-PMP/PCP mapping or manual forwarding. DDNS resolves IP, it does not punch NAT. Full CGNAT transport requires reachable rendezvous and TURN (or successful direct ICE) and infrastructure is BYO.
- Pin rotation and restoring Host installation require explicit re-pairing. Keep identity private key off backup archives unless explicitly secured; never log.
- Document a clear TLS trust UX in Tauri; don't silently turn off verification because self-signed certificate would otherwise fail.

## Review questions

- Verify Rust TLS backend actually supports Ed25519 certificate/SAN generation and native Windows Tauri hook; if not, adjust key-binding construction **before implementing**. Do not silently replace with insecure trust-all cert behavior.
- Verify webrtc-rs/runtime can expose/validate remote DTLS fingerprint versus signed Host identity and implement the stated transcript robustly; manual security review required. Protocol proposal does not constitute a cryptographic proof.
- Confirm exact domain separation/canonical byte encoding for signatures (recommended length-prefixed binary + fixed context string), not free-form JSON canonicalization.
