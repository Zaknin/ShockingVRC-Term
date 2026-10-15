# 22. Host Remote Server — validation, diagnostics and rollout plan

**Status: PROPOSED FOR REVIEW (v0.16)**. Tests below are required *plans*, not passing evidence. Baseline previously confirmed on Windows: 137/137 upstream `cargo test --workspace` at `e076bc2`.

## 1. Suggested validation layers

| Layer | Test methodology | Pass condition |
|---|---|---|
| Schema | Validate every v1 request/response against approved schemas; compare against golden examples | No drift and no undocumented permissive fields |
| Backend adapter | Fake Bridge/`Bridge::detached()`, inspect receiver | 37 mapped ops enqueue correct original `Command` variant; updater unreachable |
| Snapshot DTO | Fixed simulated `Bridge::Snapshot`, `Shared`, catalogs | No missing required fields, no guessed values or debug serialization |
| Auth/session | Deterministic time + fake transport | One slot, valid resume, generation fences, kick/revoke behavior |
| Direct listener | Loopback HTTPS/WSS and real Windows bind | Authentication only over verified TLS; OFF=no listening port |
| REST | Test all approved endpoints with same shared state | REST cannot circumvent operator slot or caps |
| Liveness/reconnect | Packet drop, stale connection, clock advance | Heartbeat 5s, detect ~15s, 30s RESERVED; no stale mutations |
| NAT/mapping | Consumer router / UPnP support and unsupported router | Lease own mapping only; manual fallback; no effect on Host runtime |
| Full E2E | Windows Host + Tauri Terminal + device where appropriate | Both UIs show same values; device baseline still works |
| RTC (later milestone) | BYO rendezvous/STUN/TURN, restrictive NAT/CGNAT simulation | Same v1 API; signed DTLS identity; relay fallback |
| Regression | `cargo test --workspace`, GUI visual/smoke, disconnect cycles | 137 existing pass and new remote tests pass; Worker tick unchanged |

## 2. Security/credential tests — fail closed

- TLS self-signed server presents mismatched SPKI: native Terminal refuses **before** sending credentials; never use `danger_accept_invalid_certs(true)` without a strict, tested verifier (and prefer not exposing such global bypass).
- Host first-pair fingerprint mismatch against local Host display: deny. Key renewal/certificate changes with same identity work only if approved binding proven; identity key replacement requires explicit re-pair.
- Wrong/invalid login, busy slot, invalid bearer, unexpected `session_id`, stale generation, replay with different body, consumed ticket outside 15s, malformed JSON, duplicate JSON keys, oversized frame, NaN/Inf, recursive mod function depth > allowed: typed reject, no enqueue.
- Rate limiter prevents unauthenticated Argon2 resource exhaustion and mutation flood. Anonymous `/health` leaks no connected devices, usernames, policy, IP or secrets.
- Invalidate all tokens/tickets on kick, Remote OFF, password change, restart and grace expiration; `session.revoked` is best effort but Host rejection is authoritative.
- Verify no secrets in saved JSON, logger ring, panic messages, tracing, URL query, browser/WebView storage, fake `Debug` or sanitized `logs.get` result.
- RTC: verify cryptographically bound peer fingerprint, swapped SDP, tampered signed proof, malicious signaling/TURN impersonation attempts; STOP the implementation path if conditional ADR-033 cannot be met.

## 3. Mutation and cap race tests

1. `Host cap disabled`: remote `A/B:min/max` in normal `0..200` passes, same as local behavior.
2. `Host cap enabled A70 / B120`: A71 rejected, B120 allowed, B121 rejected. No policy bleed across channels.
3. Host local A max=150 while remote cap A70: remote gets reported `150` unchanged. Remote write A60 allowed where compatible with current min; remote write A151 rejected.
4. Remote applies preset/zone config/alarm changes with cap enabled: **no new cap rejection**. Assert algorithm unchanged.
5. Host changes cap during remote command race: gateway admission is ordered relative to Host policy commit; no stale precomputed client-side cap grants authority.
6. Host kick during command ingress: admission before fence may enqueue; any after fence MUST NOT enqueue. Parallel WSS and REST must not bypass fence.
7. Resume from WSS to RTC: old WSS generations cannot enqueue; current RTC only. Return from RTC to WSS same behavior.
8. Target device switched locally while remote sends index-based zone operation: document existing upstream race and do not claim atomic per-device isolation; test best-effort precheck where implemented.
9. Proven enqueue vs actual Worker application: simulate closed channel -> no `command.accepted` queued=true; successful enqueue followed by Worker no-op must not generate `applied=true`.
10. Repeated request-id/payload: cache accepted once; different payload same ID => conflict; after outage client doesn't auto-replay uncertain writes.

## 4. Service start/stop tests

| Case | Expected observation |
|---|---|
| Fresh install without credentials | Remote OFF; no TCP port, no NAT mapping, no outbound rendezvous |
| Remote ON + invalid credentials/config | Fail closed, show Host GUI ERROR; local ShockingVRC remains operational |
| Remote ON + valid config | One TLS listener, one backend/Bridge, authenticated API operational |
| Port 4318 already bound | Clear bind error; no crash/no change to core; mapping not created |
| Mapping unavailable or CGNAT | Local API still reachable; mapper DEGRADED; manual forwarding offered |
| Toggle OFF while ACTIVE/RESERVED | Fence/revoke, listener stopped, own port mapping removed, Engine unchanged |
| Host password changed | Existing session revoked and fresh login required |
| Host exits while client connected | Bounded task shutdown/lease cleanup before existing runtime ends |
| Minimize/restoring window | Remote endpoint and existing Worker remain alive; FPS not part of remote tick |
| NAT lease changes/expires | Diagnostic status correct, lease renew/release only own mapping |

## 5. Metrics and Host-only diagnostics

Remote panel should expose, without storing secrets: remote server enabled/listening/error, listen address/port, TLS fingerprint, pairing state, mapping protocol + port + lease expiry, operator slot FREE/ACTIVE/RESERVED, transport kind, last authenticated activity, connection generation, reconnect counters, published snapshot rate and approximate egress bandwidth. For diagnostics, map error codes to readable Host messages; don't echo password/headers/tickets.

Only one actual remote session is allowed; optionally bound unauthenticated transport connections separately to avoid overload. Track egress backpressure and count snapshots coalesced/dropped rather than grow queues indefinitely.

## 6. Acceptance gates by implementation slice

**Gate H0 — identity feasibility:** prove Windows TLS verifier/Ed25519 SPKI binding and RTC negotiated DTLS proof strategy; document algorithms, pairing flow, negative MITM tests. ADR-033 needs feasibility/security review, never automatic downgrade. No remote password or control path before passing direct identity verification.

**Gate H1 — backend/gateway:** 37 operations mapped, correct enqueue Result, strict v1 DTOs and cap policy tested, fake backends; upstream 137/137 still pass.

**Gate H2 — authentication+direct server:** one-slot state, REST+WSS parity, kick/revoke, resume cache 15s, TTL 900s, heartbeat 5/15/30, start/stop and Windows listener. Evidence includes bad credentials, bad TLS, stale generation and oversized messages.

**Gate H3 — local GUI integration:** Remote settings, authenticated pairing fingerprint, session diagnostics, mapping state, local Host unaffected, no listener/ports while OFF.

**Gate H4 — RTC transport:** self-hosted signaling+coturn installation examples, no shipped defaults, genuine WebRTC ordered DataChannel, verified signed DTLS proof, STUN/TURN fallback including simulated/real CGNAT, cross-transport resume and auth/cap parity.

**Gate H5 — Tauri Terminal E2E:** separate client architecture and UX plan, actual Windows testing, symmetric live values, saved credentials in protected OS store; post-tests FPS optimization separate.

These gates are not permission to start Codex yet; they structure the later engineering prompts. At every milestone, record exact commit, test command/result, negative tests, environment and no-touch verification of `src-core`.

## 7. Evidence and publication (after review, not now)

Use existing project evidence workflow only when there is an artifact worth publishing. Proposed canonical location for approved architecture/evidence: `\\\<evidence-share>\Torrents\Codex\ShockingVRC-Term\...`, independently read-back and SHA256 verify with sidecar written last; Google Drive `/Codex/ShockingVRC-Term` is downstream mirror, not canonical. Current v0.16 remains a **local draft ZIP** unless explicitly published. Do not claim tests or deployments that have not occurred.
