# 18. Remote API v1 — APPROVED decisions (ADR-028–038)

**User approval:** 2026-10-15. **Scope:** the full v0.14 proposed API package was approved. These rows are design acceptance, **not implementation or a security audit**. The crypto mechanism ADR-033 is conditional on feasibility verification; if it fails, stop and propose an alternative for separate approval instead of weakening identity verification.

| ID | Decision | Status |
|---|---|---|
| ADR-028 | Versioned UTF-8 JSON frames: WSS text and ordered/reliable WebRTC DataChannel; strict validation, no HTTP emulation over RTC | ACCEPTED |
| ADR-029 | Full direct HTTPS REST companion; canonical Tauri WSS in-band auth; REST shares one session slot | ACCEPTED |
| ADR-030 | Access token TTL=900s, refresh; volatile resume ticket; idempotent resume replay cache 15s/8 entries, retries 1/2/4s; command dedup 120s | ACCEPTED |
| ADR-031 | Initial full Snapshot; network coalescing 5–10Hz, max 10Hz; upstream 50-ms bridge_revision is not a config CAS | ACCEPTED |
| ADR-032 | Stable wire DTO/enums; 37 upstream-mapped operations in remote whitelist; updater excluded | ACCEPTED |
| ADR-033 | Long-lived Ed25519 Host identity, verified pinned SPKI for WSS; signed DTLS fingerprint proof for RTC; out-of-band pairing, no silent TOFU | CONDITIONALLY ACCEPTED — requires crypto/library feasibility prototype and security review |
| ADR-034 | Typed errors, one remote session across transports; secrets inaccessible from WebView | ACCEPTED |
| ADR-035 | command.accepted only after proven enqueue; additive Bridge send-result adapter allowed, no physical/apply ACK | ACCEPTED |
| ADR-036 | TCP 4318 as API default; network Snapshot max 10Hz; JSON max 256KiB; command rate up to 30/s; no preconfigured external signaling/STUN/TURN | ACCEPTED — benchmark/operational tuning can be separately proposed |
| ADR-037 | REST login PENDING_ATTACH=15s with REST-only session heartbeat; no second session slot | ACCEPTED |
| ADR-038 | Upstream index selection semantics retained; no atomicity guarantee or modification to Worker::apply | ACCEPTED |

## Retained approved decisions

- ADR-001–027 remain accepted, including original egui baseline, no modification of `src-core`, one remote operator, two independent Host-only A/B remote Power Limits edit caps (master OFF by default), and 5s / ~15s / 30s session timing.
- ICE/STUN/TURN is a required project capability but implemented in a separate milestone; signaling/STUN/TURN endpoint defaults are empty. The private Moscow OpenShock VPS is an installation example only and is not a shipped default.

## Delivery status / hard gates

- Rust/Tauri code, Host API and deployment **not implemented** by approval alone.
- `schemas/*.json`, `schemas/*.yaml` and fixtures were statically validated in v0.14; they are design artifacts rather than generated runtime behavior.
- Before exposing commands or credentials: validate WSS identity, RTC DTLS binding, session generation fencing, authorization and schema rejection with integration tests; do not fall back to trust-all TLS.
- Host Remote Server architecture and Tauri Terminal implementation design are the next review topics.
