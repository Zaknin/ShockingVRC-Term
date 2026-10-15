# 32. Direct / DDNS / ICE / TURN connectivity manager

**Status:** DESIGN FOR REVIEW. This document chooses connection **routing**, not Remote API behavior. No server defaults are populated.

## Config (per Terminal profile; the Host has corresponding enabled transport settings)

- `direct_endpoint`: optional `host:port` or TLS `https://` URL, accepts IPv4 / DNS / DDNS / IPv6 as network backend supports; never assume hostname is cryptographic identity.
- `rendezvous_url`: blank unless explicitly set by user.
- `stun_urls`: empty array default; `turn_servers`: empty array default; no bundled public providers.
- `mode`: proposed `auto | direct_only | traversal_only`; default **auto** (selection preference, not a public endpoint).
- Separate Host profile identity pin; same Host can change IP/DDNS without changing identity.

## Auto selection (proposed deterministic policy)

1. If Direct endpoint exists and is enabled, attempt TCP/TLS handshake with trust verification and WSS API health / `hello` within a short configurable deadline (proposed 3–5s; retain log of the exact stage).
2. If Direct succeeds and Host identity matches, continue. **Do not attempt ICE simultaneously** for the sole v1 session.
3. If Direct unavailable/timeout and a user-configured rendezvous exists, attempt signaling and ICE, with local, srflx (if configured STUN) and relay (if TURN) candidates.
4. Prefer verified direct ICE pair; if none, use verified TURN relay. If no available candidate pairs, show actionable failure instead of claiming that STUN opened a port.
5. If no fallback service is configured, fail with a clear `DIRECT_UNREACHABLE` / `TRAVERSAL_NOT_CONFIGURED` diagnostic. Manual VPN/DDNS address remains an option.
6. Transport changes during active session are handled as recovery **with the same session ID and current resume_ticket**, not a second login and not replay of unknown unacknowledged commands.

## Fast connection state model

`DISCONNECTED → RESOLVING → TLS_CONNECTING | SIGNALING → HOST_ID_VERIFICATION → AUTHENTICATING → SYNCHRONIZING → ONLINE`, with side states `RECOVERING`, `SESSION_BUSY`, `REVOKED`, `FAILED`. On short loss, resume via approved 5s heartbeat, ~15s detection, 30s reserved window. Full Snapshot always replaces cached state after successful resume.

## Diagnostics for UI and logs

| Stage | Explainable failure |
|---|---|
| DNS | DNS failure, no address, timed out |
| Direct | refused TCP, TLS certificate mismatch, WSS upgrade failed, timeout |
| NAT mapping | mapping created but WAN reachability unverified; CGNAT suspected; not synonymous with connected |
| Rendezvous | URL empty, TLS mismatch, unauthorized registration, Host offline, signaling timed out |
| ICE | candidates gathered, selected pair type, checks exhausted, STUN unavailable |
| TURN | credential expiry/auth reject, relay ports blocked, allocation failed |
| Auth/Session | login failed, `SESSION_BUSY`, `SESSION_REVOKED`, resume expired, protocol mismatch |
| Application | command accepted, rejected, current full Snapshot and status revision |

UI labels: `Direct WSS`, `ICE P2P`, `TURN Relay`, and `Offline`; show **actual selected path** rather than inferred from configured servers. Diagnostic exports redact tokens, passwords and TURN secret.

## DDNS

User can type a DDNS hostname in v1. Name resolution follows standard OS resolver; on reconnect re-resolve rather than caching the old IP forever. The host's identity pin remains separate. Integrated DDNS updater and ACME are later optional enhancements, not mandatory to connect by DDNS. If public cert is configured, normal CA trust is possible only when consistent with approved identity pin policy. No silent substitution with a different Host when DDNS target moves.

## Policy on configuration errors

Direct path remains available if rendezvous is misconfigured. Failure to obtain UPnP mapping never disables listener on LAN/VPN. TURN failure cannot alter Host's local active device. `Remote Access OFF` disables all remote listeners/registration and new remote session admission; it does not touch the engine.
