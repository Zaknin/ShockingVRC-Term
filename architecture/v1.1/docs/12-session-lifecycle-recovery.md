# 12. Remote Session Lifecycle & Recovery — DRAFT v0.13

**Scope:** ShockingVRC-Term fork, `egui` @ `e076bc2`. This document is a **design proposal**, not implemented behavior. The **30-second reservation after detected connection loss** is accepted (ADR-022); **5-second heartbeat and approximately 15-second dead-peer detection** are also accepted (ADR-023). Resume ticket, atomic transport fencing and Host kick semantics are accepted as ADR-024–027; specific wire fields and Host UI presentation remain draft. The one-remote-session limit, non-takeover rule, local egui independence and immediate state synchronization are **already accepted** (ADR-017, ADR-018).

## 1. Accepted invariants

1. Maximum **one active authenticated remote controlling session per Host**, shared between WSS and WebRTC DataChannel (ICE/STUN/TURN). Native egui does not consume the slot.
2. A new unrelated login while occupied returns `SESSION_BUSY` and does **not** evict the existing Terminal, even if it supplies the correct username/password.
3. Host is authoritative (`Bridge` + `Worker`); both UIs see actual shared state promptly. Transport disconnection never undoes already-queued commands or stops local ShockingVRC BLE/OSC/Alarm/output.
4. Host can explicitly revoke the remote session; Remote Access OFF prevents new remote sessions. No new direct access to `src-core`, no new engine power governor.
5. Both transports share one session manager, credential authority, lock and revocation behavior. Changing WSS↔RTC cannot grant a second operator slot.

## 2. Accepted Host state transitions (wire design pending)

```text
REMOTE_DISABLED --Host enables--> SLOT_FREE
SLOT_FREE --verified identity + successful auth--> ACTIVE(session_id, epoch, generation)
ACTIVE --short transport loss/heartbeat timeout--> RESERVED(session_id, until)
RESERVED --same session proves resume ticket before expiry--> ACTIVE(new generation)
RESERVED --grace expires--> SLOT_FREE
ACTIVE/RESERVED --logout or Host revoke--> SLOT_FREE
ANY --Host Remote OFF--> REMOTE_DISABLED
ANY --Host restart/password change--> fresh auth required, prior sessions invalid
```

**Design principle:** session ID is not the network socket or ICE connection ID. Session membership is under a single Host session-manager mutex/atomic critical section, regardless of transport.

### Timing — accepted vs proposed

| Knob | Value | Status | Meaning |
|---|---:|---|---|
| Heartbeat | 5 s | **ACCEPTED — ADR-023** | Authenticated app-level ping/pong or equivalent liveness check, usable across WSS and RTC |
| Dead-peer detection | ~15 s | **ACCEPTED — ADR-023** | Approximately 15 s without authenticated liveness response (nominally three missed 5 s heartbeats); mark remote temporarily disconnected |
| Reconnect reservation | 30 s | **ACCEPTED — ADR-022** | Reservation begins **after detection** of transport loss, not at initial packet loss |
| Reconnect attempts | 1s / 2s / 4s auth.resume retries; bounded within 30s grace | **ACCEPTED ADR-030** | Terminal tries new transport while grace remains; no automatic replay of commands |

The accepted heartbeat and detection timings are nominal; scheduling/network jitter can change wall-clock detection latency, while explicit clean transport closure can be detected sooner. The 30 s grace starts at **actual loss detection**, not necessarily 15 s after the last packet. Heartbeat and detection are independent of the upstream 50 ms Worker ticker. An explicit clean `auth.logout` is not a transient loss: free the slot immediately.

## 3. Authentication, reconnect and fencing — ACCEPTED behavior

- Ordinary username/password login is allowed **only when SLOT_FREE**, after Host identity verification. A valid password **does not authorize takeover** from an occupied slot. Auth errors should avoid exposing secrets in logs.
- Successful login creates an opaque cryptographically random `session_id` and an independent secret **resume_ticket**. `session_id` is not sufficient for authentication. Server stores a digest or otherwise validates the ticket; ticket never appears in URL/query/logs/renderer localStorage.
- Resume is an explicit `auth.resume` operation after Host identity verification. It must prove the original session's resume ticket; having the same Host username/password is **insufficient** for taking over the reservation.
- The resume ticket is **scoped to server_epoch** (specific Host run), expires with the reservation, and is invalidated by logout, Host revoke, Remote OFF, password change and restart. Rotate ticket on successful resume. Keep in Tauri Rust memory for this running Terminal process, not in Vue or persistent config. Saved Host username/password in OS credential storage remains a separate facility for **new login** once slot is free.
- If the old socket is still half-open during valid resume, Host **atomically replaces** the session's `transport_generation`, closes/fences the old connection, and accepts commands only from the new generation. Never allow old and new connections to act concurrently.
- A reconnect can use the same or a different configured transport (WSS/RTC) when it proves the ticket and verifies the same pinned Host identity. Sharing a common slot does not imply silently falling back to unconfigured public signaling/STUN/TURN endpoints.
- After every successful resume, send **full authoritative Snapshot, capabilities, Remote policy and current catalogs**, then resume live events. Sequence counter restarts or is re-established in a documented way; the client must not merge stale updates from previous transport generation.
- Requests sent near disconnect have uncertain fate. **Never replay mutating requests automatically**, even if the last ACK was lost; refresh state instead. A command ACK remains receipt/enqueue only, not hardware execution.
- Once `RESERVED` expires, re-entry requires a normal new login. If a new Terminal already occupied the free slot, old Terminal receives `SESSION_BUSY`, not forced takeover.

Illustrative frame shape (**not yet frozen in schema**):

```json
{"type":"auth.resume","protocol":1,"request_id":"<UUID>","session_id":"<opaque>","resume_ticket":"<secret>"}
```

## 4. Host-initiated actions and client UX — ACCEPTED policy, UI draft

| Host action | Server behavior | Terminal behavior |
|---|---|---|
| **Disconnect current session** | Revoke ticket/token, fence transport and immediately free remote slot | Show `SESSION_REVOKED`; **do not silently auto-login** using stored password; explicit user action needed |
| **Remote Access OFF** | Revoke all active/reserved sessions; close WSS listener/signaling/RTC connections; disable login | Show `REMOTE_DISABLED`; retry only if user explicitly restarts connection or re-enables setup |
| **Change Host password** | Invalidate all sessions and tickets | Re-enter updated password to login again |
| **Restart Host** | Change `server_epoch`, clear volatile session state | Reconnect by verifying same Host identity, then fresh login + full snapshot; old ticket is invalid |
| **Host egui stays open** | Continuous local state/control, independent of remote slot | Remote control does not override Host authority |

**Host UI proposal:** `Remote Access` enabled indicator; session state `Available / Connected / Reconnecting (reserved) / Disabled`; connected client's nickname / connection transport (WSS or RTC/TURN) / connection duration / last activity; buttons `Disconnect client` and `Disable Remote Access`. Do not show secrets or persist a public server URL by default.

**Security nuance:** "Disconnect current session" is a kick, not a permanent ban. A client with valid credentials can explicitly log in again if the slot is free. To block future logins, Host uses `Remote Access OFF` (or changes credentials). Client auto-retry after `SESSION_REVOKED` is intentionally suppressed, but malicious clients are not prevented from trying again with valid credentials.

## 5. Terminal state and error handling — ACCEPTED policy, UI draft

```text
DISCONNECTED -> CONNECTING -> VERIFYING_HOST -> AUTHENTICATING -> SYNCING -> ONLINE
ONLINE -> LINK_LOST -> RECONNECTING -> VERIFYING_HOST -> RESUMING -> SYNCING -> ONLINE
RECONNECTING -> GRACE_EXPIRED -> DISCONNECTED (fresh login possible)
ANY -> SESSION_REVOKED / REMOTE_DISABLED / HOST_IDENTITY_MISMATCH -> terminal stop
```

- In `ONLINE`, controls show true Host values (including power above remote-only cap); Host-only policy is read-only to remote.
- In `LINK_LOST`/`RECONNECTING`, display last known Snapshot as **stale**, freeze remote commands, and explain that Host still operates locally. Avoid false "0 power" indicators and do not infer an emergency shutdown.
- On restored link, discard pending optimistic edits and refresh full state. If a new Host epoch is observed, reauthenticate, not resume across reboot.
- `SESSION_BUSY`: display occupied session, no automatic preemption. A queued wait-to-connect mode is not part of v1.
- Invalid pinned TLS/DTLS identity: fail before sending password or ticket; never silently trust changed Host key.
- Explicit logout from Terminal and closing Terminal via controlled shutdown should send `auth.logout` when possible; abrupt process death follows liveness/grace path.

## 6. Protocol implications (behavior accepted; not yet final wire contract)

Suggested additional message types/codes:

- `auth.resume` / `auth.resume.result` (independent proof and rotated ticket after success)
- `session.status` (e.g. `active`, `reserved`, `free`; private detail only on Host GUI)
- `server.notice` code `SESSION_REVOKED`, `REMOTE_DISABLED`, `HOST_RESTARTING`
- `command.error` or connection error `SESSION_NOT_ACTIVE`, `SESSION_BUSY`, `SESSION_EXPIRED`, `RESUME_FAILED`

The behavior is approved in ADR-024–027; the existing schema/OpenAPI remain **draft** and are the next deliverable to extend. REST login and in-band WSS login must use exactly the same session reservation service; do not count HTTP token and its associated WSS stream as separate operators.

## 7. Verification matrix

| Case | Expected |
|---|---|
| WS drop; same Terminal resumes using valid ticket within grace | Slot retained; full Snapshot; new generation solely active |
| Second Terminal has correct password during ACTIVE or RESERVED | `SESSION_BUSY`, no eviction |
| Valid resume while old transport half-open | Old transport fenced and unable to enqueue commands |
| WSS drops, resumes through configured RTC | Same one slot and same Host identity, no duplicate sessions |
| Resume ticket stolen/expired/invalid; same password | Cannot hijack RESERVED session; no state leaked |
| Grace ends | Slot free 30 s after detected loss; subsequent normal login may claim it |
| Three missed 5 s heartbeats (approximately 15 s of no liveness) | ACTIVE→RESERVED; start the separate 30 s grace timer; no commands accepted on stale transport |
| Healthy peer responds to the 5 s heartbeat | Remains ACTIVE; does not enter RESERVED |
| Host kicks active/reserved client | Tokens dead; terminal does not automatically re-login; slot free |
| Remote OFF or password change | Tokens dead; no old-credential continued control |
| Host restart | `server_epoch` changes, old ticket invalid, fresh login required |
| Drop at command boundary | No automatic replay; following Snapshot determines current state |
| Network partition during actual locally-driven output | BLE/OSC/Alarm and Worker unchanged; remote UI stale |
| WSS login + RTC login race | Session manager admits at most one remote operator |

## 8. Approved behavior; wire contract pending

**Accepted:** reservation after loss detection 30 s (ADR-022); 5 s heartbeat and ~15 s detection (ADR-023); ticket-based same-session resume, transport generation fencing, Host kick, secret storage and bounded idempotent replay of lost resume ACK (ADR-024–027). See `13-session-auth-revocation.md`.

**Not frozen:** exact message schemas, token TTL/refresh, replay-cache bounds and Host identity cryptographic binding. Re-send of mutation commands remains prohibited. The `auth.resume` duplicate response retry reuses original `request_id` and does not create a new generation.
