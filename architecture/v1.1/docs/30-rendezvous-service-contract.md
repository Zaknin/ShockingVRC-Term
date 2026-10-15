# 30. Rust Rendezvous Service — internal contract

**Status:** PROPOSED for implementation review; the owner has selected **a self-hosted Rust signaling service**. It is deliberately separate from `src-remote` and `src-terminal` and never processes device commands.

## Source layout (proposed)

```
src-rendezvous/
  Cargo.toml
  src/{main,config,server,routes,registry,rooms,signaling,authorization,turn_credentials,metrics}.rs
  tests/{registration,exchange,expiry,turn_credentials,interop}.rs
```

Target stack: Rust + Tokio + Axum (TLS termination via rustls or an explicit operator reverse proxy), `serde`, CSPRNG, HMAC-SHA1 for coturn REST ephemeral TURN credentials (as required by TURN REST convention); no database needed for initial personal/friends instance. Store configuration and TURN REST shared secret **server-side** only. Library versions resolved and locked at implementation time. `/healthz` returns only process readiness; `/version` exposes protocol version, no room directory.

## Roles and trust boundary

- `HostRegistrant`: outbound persistent WSS to configured rendezvous, proves authorization to register an **exact** self-hosted server's stable identity; identity's public key is NOT a password.
- `TerminalJoiner`: outbound signaling WSS; sends room/server locator and ephemeral request ID. Room locator is not sufficient to authenticate **application login**.
- `Registry`: maps stable public host ID → one active registered signaling connection (latest authorized registration wins by generation); **never stores private Host identity key**.
- `Room`: one ephemeral offer/answer/ICE exchange; has session nonce, short TTL, max message cap, pair of authorized peers; rooms automatically expire.
- `TurnCredentialIssuer`: only after authenticated/authorized rendezvous exchange, issues coturn-compatible short-lived credentials from the **server-held** shared secret. Neither shared secret nor global coturn admin password is sent to peers.

## Signal wire namespace (independent of approved Remote API)

This namespace is **only** for ICE negotiation. Do not change `schemas/remote-messages-v1.schema.json` for it. Contract detailed in `../schemas/rendezvous-v1.schema.json` and `../examples/rendezvous/`.

| Message | Sender → receiver | Meaning |
|---|---|---|
| `register` / `registered` | Host → rendezvous | Register Host identity/public ID + tenant authorization, server registration epoch |
| `join` / `join.accepted` | Terminal → rendezvous | Request a one-time exchange with named Host |
| `signal.offer` | Terminal → Host (or negotiated role) | SDP offer in bounded envelope |
| `signal.answer` | Host → Terminal | SDP answer in same room |
| `signal.candidate` | Either → other | ICE candidate/trickle event |
| `signal.end_of_candidates` | Either → other | Completion marker |
| `turn.credentials.request/result` | Peer ↔ rendezvous | Short TTL coturn username/password/URI for this server |
| `room.close` / `error` | Either / server | Expire exchange or specific typed failure |

`register` authorization: propose operator-generated random **registration key** for the allowed Host profile, provisioned individually into Host configuration. Join authorization: propose separate **per-host invite/pairing token** supplied out-of-band (must not be just an enumerable public `server_id`). Both are independent of ShockingVRC username/password. These are design details; if a lighter implementation still meets authenticated room routing, record the change in an ADR instead of silently changing approved Host-auth flow. No mandatory cloud login/account.

## Rooms and lifetime

- Registry entry while Host WSS alive + lease/heartbeat; expiry on close/timeout.
- At most one pending negotiate per stable Host ID at a time for the initial reference instance; this is **not** the application one-slot manager, which remains authoritative on Host.
- Proposal: room TTL **60s**, maximum candidate count **64** per peer, signaling frame ≤**64 KiB** per envelope, no unbounded buffers. These three are adjustable and **not approved Remote API constants**.
- Rejoin room creates fresh nonce/room ID. Do not interpret successful `join` as access to Host.
- On rendezvous restart, ephemeral state is lost; peers can retry. Existing established direct/RTC data channel remains Host-managed.

## Negotiation and identity

Signaling server can misroute or modify messages; host identity verification is in Terminal's Rust layer and must bind negotiated DTLS certificate fingerprint and nonces to the user's already paired Host key (conditional ADR-033). Rendezvous never claims to verify that for the Terminal. The exact signature encoding and compatible library hooks must be proven in an isolated prototype before enabling this path with Host credentials.

## TURN REST credentials

For coturn `use-auth-secret` mode: short-lived `username = <UNIX expiry>:<opaque member ID>`, `password = Base64(HMAC-SHA1(static-auth-secret, username))`; server clock and coturn clock must agree. Configure static secret outside source/Git/ZIP; recommend initial TTL **600s** (PROPOSED) and renewal before expiry if relay allocation lifetime requires it. TURN credential renewal is **independent of** 900s Remote API bearer TTL. `turn.credentials.result` is delivered to the authenticated requester for this rendezvous, not published as a public catalog.

## Operator ownership and diagnostics

Settings expose public URL, optional TURN/STUN URIs, realm, secret references, allowlisted Host registrations; no defaults. Logs show room creation/closure, connection counts and errors, not passwords, TURN shared secret, raw long-lived tokens or unredacted SDP. If address/ports impossible, error contains concrete `rendezvous_unreachable`, `room_not_found`, `room_busy`, `room_expired`, `auth_required`, `unsupported_version`, `turn_unconfigured`, `turn_auth_failed`.

## Non-destructive server tests

- Fake Host and Terminal using generated ephemeral test keys and loopback service; never invoke real `Bridge`.
- Valid host register/join/offer/answer/candidates; malicious/unknown room rejected; candidate limit enforced; server restart clears registry.
- Coturn ephemeral credentials reproduce independent HMAC vectors and expire at configured timestamp.
- Neither side can use signaling messages to execute Remote API `command`.

## v1.0.2 — type-specific required wire fields (F-04)

`schemas/rendezvous-v1.schema.json` now uses `oneOf` keyed by `type`. Every message requires `protocol`, `type`, `request_id` plus:

| Type | Additional required fields |
|---|---|
| `register` | `host_id`, `identity_public_key`, `authorization` (Host registration key) |
| `registered` | `host_id`, `registration_epoch` (UUID) |
| `join` | `host_id`, `authorization` (per-Host invite token) |
| `join.accepted` | `host_id`, `room_id`, `nonce` |
| `signal.offer` / `signal.answer` | `room_id`, `sdp` |
| `signal.candidate` | `room_id`, `candidate` |
| `signal.end_of_candidates` | `room_id` |
| `turn.credentials.request` | `room_id` |
| `turn.credentials.result` | `room_id`, `turn_uris`, `username`, `credential`, `expires_at` |
| `room.close` | `room_id` |
| `error` | `code`, `message` |

The `registration_epoch` is the current accepted Host registration instance UUID, **not** the Remote API `server_epoch`; changes when a new host registration replaces the prior one. These signaling details remain **proposed** until the infrastructure design is accepted; no modification of approved `remote-messages-v1` protocol namespace.
