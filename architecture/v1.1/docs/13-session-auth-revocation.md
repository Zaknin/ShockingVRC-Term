# 13. Session Authentication, Resume & Host Revocation — DRAFT v0.13

**Design status: BEHAVIOR ACCEPTED (ADR-024–027); wire contract and cryptographic binding NOT YET FROZEN.** Applies to fork `Zaknin/ShockingVRC-Term`, source `egui` commit `e076bc2`, API protocol v1 on either native WSS or WebRTC reliable ordered DataChannel. This document **extends** previously accepted decisions ADR-017–ADR-023. See `12-session-lifecycle-recovery.md` for accepted 5 s heartbeat, ~15 s dead-peer detection and 30 s reconnect grace after detection. **No code implementation yet.**

## 1. Trust boundaries

- Local Host `egui` is the owner/administrator. The authenticated Tauri Terminal is an operator, **not a Host administrator**. Owner-only features: Remote Access ON/OFF, credentials change, session kick/revoke, `Remote Maximum Strength` A/B policy.
- Host `Bridge`/`Worker` remains the single command executor and state authority. Session manager only authorizes remote command delivery; it does not alter BLE, OSC, DSP, modulation, power engine, local Alarm or its processing intervals.
- Direct WSS must validate the pinned Host TLS identity **before** transmitting username, password, `access_token` or `resume_ticket`.
- ICE/RTC must cryptographically validate the data-channel peer against pinned Host identity and authenticated handshake/binding **before** application credentials. Self-hosted signaling/TURN servers are transports, not trusted endpoints for app credentials. The Host identity key/fingerprint binding is **conditionally accepted (ADR-033)**, not a completed or audited cryptographic implementation.
- The Tauri **Rust backend** owns secrets and network I/O; Vue/WebView receives only safe connection/status DTOs. Never include tokens or password in renderer state, URLs, logs, crash reports or diagnostic exports.

## 2. Credentials and token separation (ACCEPTED behavior; wire details pending)

| Material | Purpose | Client storage | Server storage / validity |
|---|---|---|---|
| Host username / password | Establish a **new** remote session when slot is FREE | OS protected credential store, only if user opts to remember; Rust backend | Host password salted memory-hard hash; password not recoverable |
| `session_id` | Non-secret opaque session identifier | Rust memory | Session manager, random ID |
| `access_token` | Authenticated REST/attached WSS calls **within that one existing session** | Rust memory only | Revocable session-scoped verifier; never grants second operator slot |
| `resume_ticket` | Prove membership in **exactly the same session**, even on another configured transport | Rust memory only, never persistent | Independent random bearer secret / verifier, scoped to `session_id` + `server_epoch`; invalidated on logout/revoke/expiry/password change/restart |
| `server_epoch` | Identify current Host process lifetime | Not secret | New random instance ID on every Host start |
| `transport_generation` | Fence former transport after an accepted resume | Not secret | Monotonically increasing per session; checked for all remote command submissions |

Generate bearer secrets using a CSPRNG (proposal: at least 256 unpredictable bits). All secrets are short-lived and invalidated server-side. Password and secret comparisons are performed in a timing-resistant manner as appropriate. **`session_id` or saved password alone never authorizes resume.**

No Host tokens are written to plain-text config. A user's OS-stored saved password is a convenience for starting a future *new* session; it is distinct from, and cannot steal, the currently reserved remote session.

## 3. One authoritative server-side slot

```text
REMOTE_DISABLED
  └─ Host enables ─→ FREE
FREE
  └─ Host verified + correct username/password ─→ ACTIVE(sid, epoch, gen=1)
ACTIVE
  ├─ unauthenticated second login ─→ SESSION_BUSY; remains ACTIVE
  ├─ valid ticket reattach (same sid) ─→ ACTIVE(sid, gen+1), old gen fenced
  └─ disconnect or heartbeat dead-peer ─→ RESERVED(sid, until=detected_at+30s)
RESERVED
  ├─ valid ticket before deadline ─→ ACTIVE(sid, gen+1)
  └─ deadline elapsed ─→ FREE; ticket invalid
ACTIVE or RESERVED
  ├─ logout / Host kick ─→ FREE; tokens invalidated
  ├─ password change ─→ FREE; old tokens invalidated
  └─ Host Remote Access OFF ─→ REMOTE_DISABLED
Host restart ─→ new epoch, EMPTY/FREE/DISABLED per Host config, old secrets invalid
```

All checks, admission and generation fencing use **one atomic session-manager decision** across WSS, REST and RTC. A REST bearer and a WSS stream belonging to the *same* authenticated session do not consume two slots. Multiple command-carrying transports of the same generation must not result in duplicate operators: a successful stream attachment/re-attachment fences the previous command transport.

Remote Access ON/OFF is controlled **locally**; on Host remote disable, close direct listening and RTC activity when feasible, and reject all remote operations immediately. Changing Host credentials invalidates existing sessions. Same local `egui` and same Worker continue running.

## 4. New login and active session (ACCEPTED behavior)

1. Terminal establishes direct TLS/WSS or RTC DataChannel via user-configured signaling/STUN/TURN; verifies pinned Host identity.
2. `hello` negotiates compatible version and publicly safe capabilities; Host does not expose private state or secrets to an unauthenticated client.
3. `auth.login` sends username/password. The Host atomically admits the request **only when FREE**; `SESSION_BUSY` must not evict or disclose the active operator's identity. Password validation and slot admission should be ordered to avoid leaking account validity through busy handling (implementation detail to design before code).
4. Host issues `session_id`, `access_token` (where REST/attach is used) and independent `resume_ticket`; short token expiries/refresh semantics are not yet final. Terminal receives full authoritative `snapshot`, `capabilities`, policy and catalogs before enabling controls.
5. Every command must be authenticated and scoped to the active generation; enqueue ACK is **not** execution proof, even with a successful transport response.
6. Liveness: 5-s app heartbeat; after about 15 s without valid replies transition to RESERVED; retain slot a further 30 s **from detection**. Do not change upstream Worker tick.

**REST compatibility:** `POST /auth/login` creates one session and its associated bearer; `GET /ws` with that bearer attaches to the **same** session, never re-runs a new login or uses a second slot. An unauthenticated WSS connection can instead perform in-band `auth.login` to create the slot if FREE. Server rejects a bearer from a revoked/expired session. Do not expose tokens in URL/query parameters.

## 5. Resume handshake and ambiguous outcomes (ACCEPTED behavior)

1. Terminal retains `session_id` + `resume_ticket` **only in Rust process memory**. On link loss it marks state STALE, freezes remote controls, then retries via configured transport; it does **not** resend unacknowledged state-mutating commands.
2. After verifying Host identity again, it sends `auth.resume(session_id, resume_ticket, request_id)` over the newly secured transport.
3. Host verifies same epoch, same sid, valid ticket, ACTIVE or RESERVED state, deadline not elapsed, and atomically increments `transport_generation`. The previous transport is revoked/fenced at the session gateway; packets from stale generations are rejected before enqueue.
4. Host returns a **new** `resume_ticket` and full fresh `snapshot`, policy, capabilities and catalogs. Client discards all prior stream updates and uncertain optimistic edits. `request_id` is a correlator, not proof that a command was applied.
5. Resume is valid for an ACTIVE session too, e.g. old transport is half-open after network handover, provided the ticket belongs to that same session. Valid ordinary password from a **different** Terminal cannot take over ACTIVE/RESERVED.
6. **ADR-027 ACCEPTED:** if the Host accepted resume but the rotated-ticket reply is lost, Terminal may retry **only the same `auth.resume(session_id, old_ticket, request_id)`** within a bounded replay window. Host recognizes the exact authenticated tuple, returns the prior resume result including the *same* new ticket and generation, and does **not** increment `transport_generation` a second time. The old ticket is valid solely for retrieving this cached prior result, never for a new resume operation. Revoke/logout/expiry/password change/Remote OFF/restart invalidate the cached result. The replay-cache TTL, maximum retry count and transport-reattach mechanics must be fixed in wire-schema design before coding; after replay-window expiration the Host must reject old ticket, never guess a new credential. No state-mutating command is retried automatically.

Illustrative envelope (not yet merged into official JSON Schema):

```json
{"type":"auth.resume","protocol":1,"request_id":"f860b1a8-7a31-497a-a6b3-2283d3cd46ee","session_id":"<opaque>","resume_ticket":"<secret in Rust memory>"}
```

**Acceptance semantics:** on reconnect/revoke, a command queued in the existing Worker **before** fencing may still be applied. The Host session gateway guarantees only that **no new commands from the revoked/old generation** are queued after the fencing point. Never claim remote revocation rolls back historical engine mutations.

## 6. Owner-initiated revoke and logout (ACCEPTED behavior)

| Cause | Server action | Terminal UX |
|---|---|---|
| User chooses `Logout` | Verify caller's active session, invalidate bearer & ticket, release slot immediately | `DISCONNECTED`, remembered Host entry retained if configured |
| Owner presses **Disconnect client** in local egui | Fence current generation; revoke session/bearers/ticket; release slot immediately | `SESSION_REVOKED` (best-effort notification), **no silent auto-login**; explicit Connect required |
| Owner turns **Remote Access OFF** | Revoke any active/reserved session, block new login, close server and RTC attachment points | `REMOTE_DISABLED`; no automatic recovery against disabled service |
| Owner changes Remote password | Revoke all sessions; require new credentials | `AUTH_REQUIRED`, ask to sign in with updated password |
| Host restart/crash | New `server_epoch`, volatile sessions cleared | Verify same pinned identity, **fresh login**; cannot resume old ticket |
| Unclean Terminal exit | Heartbeat detects loss then 30-s RESERVED | Normal next login only when slot FREE, unless same still-running Rust instance has ticket |

**Critical distinction:** Disconnect client is **not a permanent ban**. If client still has valid password, it can explicitly log in anew after kick when slot is FREE. To block all access Owner turns Remote Access OFF or changes password. Do not imply that client-side suppression of reconnect is an access-control mechanism.

Host local UI proposal: `Remote Access ON/OFF`, `Connected / Reserved / Free`, client nickname, transport, duration, last authenticated activity, `Disconnect client`. Password change and policy edits remain Host-only. Current-session identity, duration, nickname must be treated as client-reported/diagnostic unless actually authenticated; no client secrets displayed.

## 7. Failure modes and guarantees

- `SESSION_BUSY`: another authenticated remote operator is active/reserved; keep first session.
- `RESUME_FAILED`: wrong/expired/revoked ticket. Do not try default/fallback credentials automatically against occupied Host; wait or ask user.
- `SESSION_EXPIRED`: reservation elapsed or access token invalid; new login allowed only if slot FREE.
- `SESSION_REVOKED`: local owner invalidated the session; stop retry; require explicit Connect.
- `REMOTE_DISABLED`: owner disabled access; stop retry.
- `HOST_IDENTITY_MISMATCH`: Host pin verification failed; **do not** send password/ticket or auto-trust a new key.
- `TRANSPORT_STALE`: delayed message from a replaced generation; drop/reject before dispatch to Bridge.
- Server crashes while an authenticated remote command is in flight: outcome unknown until new login+snapshot; never infer application from enqueue ACK alone.
- Timers use monotonic Host time; reconnection allowed only while valid reservation exists. Loss detection ≈15 s may be faster on explicit socket close; 30-s accepted grace always starts at actual detection.
- Remote Access and session security are independent of Host-only Remote Maximum Strength A/B rules (Option A); policy validation must still happen before command enqueue.

## 8. Contract tests required before M1/M2 acceptance

| ID | Scenario | Expected result |
|---|---|---|
| AUTH-01 | Verified Host, correct password, FREE slot | One session created; snapshot delivered |
| AUTH-02 | Second Terminal uses correct password while occupied | `SESSION_BUSY`, no preemption, private Host state withheld |
| AUTH-03 | Same terminal with valid ticket resumes while RESERVED | Restores same session, new generation, fresh snapshot |
| AUTH-04 | Valid ticket reattaches while previous connection half-open | Old gen command fenced; at most one active command transport |
| AUTH-05 | Same password but different Terminal attempts resume without ticket | Rejected, first reservation preserved |
| AUTH-06 | Ticket invalidated by logout/kick/expiry/password change/Remote OFF/restart | Cannot resume |
| AUTH-07 | Host kick during ACTIVE or RESERVED | Immediate revoke + free slot; terminal stops auto-retry |
| AUTH-08 | Delayed command from old generation after resume/revoke | `TRANSPORT_STALE` / no fresh enqueue |
| AUTH-09 | Password/pin/ticket in logs, URL, Vue state, crash diagnostics | Never exposed |
| AUTH-10 | REST login then WSS bearer attachment | One server slot, not two sessions |
| AUTH-11 | WSS resume to RTC or RTC resume to WSS with BYO infrastructure | One session, revalidated Host identity, no duplicate operator |
| AUTH-12 | Lose ACK of accepted resume/ticket rotation | Same tuple idempotent reply inside bounded window; no second generation/ticket; after window/revoke, reject stale ticket |
| AUTH-13 | Host identity changes unexpectedly | Reject before credentials leave Terminal |
| AUTH-14 | Abrupt power loss / terminal process death | Existing Host controls unaffected; reservation expires on accepted timing |

## 9. Status and remaining protocol design

**Accepted 2026-10-15:** ticket-based resume + atomic transport fencing (ADR-024); immediate Host kick with explicit re-login only (ADR-025); credential OS-vault opt-in and volatile session secrets (ADR-026); bounded idempotent duplicate `auth.resume` response when its ACK is lost (ADR-027). All prior ADR-017–023 remain unchanged.

**Not yet finalized:** exact JSON Schema/OpenAPI messages and errors; token TTL/refresh and session attach mechanics; replay-cache TTL/size and bounded retries; Host identity cryptographic binding to both TLS and WebRTC; concrete cryptographic format/algorithms; detailed UX. These details must be resolved and tested before implementation, without silently expanding command privileges.
