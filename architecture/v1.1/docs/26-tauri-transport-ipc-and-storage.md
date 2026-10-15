# 26. Tauri Terminal — Rust transport, IPC, credentials and pairing interfaces

**Status:** PROPOSED v0.17; normative Remote API v1 remains `14-*`, `15-*`, `schemas/remote-*v1*.json`. The pseudo-interfaces below are architectural sketches, not compiled Rust code. Host architecture HOST-P01–P09 is still awaiting separate approval.

## 1. Tauri IPC contract (renderer ↔ Rust backend)

The Vue frontend calls a small typed facade instead of owning a network WebSocket or decrypting tokens. Recommended allowlist (names are **client-local IPC names**, not new Host API operations):

| Tauri command | Arguments | Response / effect |
|---|---|---|
| `list_profiles` | none | Nonsecret profile metadata array |
| `save_profile` | Label, host, port, transport, optional user-configured ICE endpoints, optional **rendezvous invite-token credential reference** (not plaintext profile field) | Stored profile ID or structured validation error |
| `delete_profile` | ID | Remove profile and associated saved credential/pin by explicit user action |
| `connect_profile` | ID | Begin identity check and transport establishment; returns state |
| `pair_host` | Profile ID and verified fingerprint/user confirmation | Persist allowed Host identity pin |
| `login` | Username, ephemeral password, optional remember flag | Establish one Host session, then full sync |
| `disconnect` | logout=true/false | Explicit logout or network disconnect according to user's selected intent |
| `retry_connect` | none | Retry verified connection; never override REVOKED lockout |
| `submit_operation` | typed `{op,args,request_id}` | Actual command.accepted/error from Host, not apply ACK |
| `fetch_state` | none | Full current Host snapshot / async refresh |
| `fetch_catalog` | kind | Presets or zone-presets |
| `fetch_logs` | cursor and bounded limit | Log page |
| `read_capabilities` | none | Host capabilities and policy |
| `forget_credentials` | profile ID | Delete OS-saved secret reference |

The frontend should not receive `resume_ticket`, `access_token`, raw password hash, long-term Host private material, TURN server administrative secrets, or opaque HTTP authentication headers. The `login` command necessarily transports a user-typed password **once** across the local WebView→Rust IPC boundary; Rust handles network encryption and optionally OS credential write. Avoid debug logging request args for this IPC command.

### Streaming

- Tauri **Channel** is first-choice for recurring `snapshot`, `remote.policy`, `logs.append`, `server.notice`, `catalogs.changed` notifications; Tauri events acceptable for low-rate connection/session status. Tauri docs explicitly distinguish channels from events for streaming.
- Frontend subscribes exactly once per app lifecycle, unsubscribes on cleanup, and can re-subscribe after WebView reload. On subscription UI receives the current full snapshot before incremental signals or a resync requirement.
- Bound internal queue; latest-state-wins for snapshots, ordered delivery for stateful protocol control messages/errors. Never allow 10Hz full snapshot duplicates to accumulate indefinitely in background.
- Version tag each IPC payload or define strongly typed TypeScript discriminated unions generated from schema; unknown event subtype logs a nonfatal diagnostic, unknown Host API mutation never forwarded.

### Illustrative pseudocode

```rust
// Not the Host API; Tauri IPC facade sketch.
#[tauri::command]
async fn submit_operation(
    state: tauri::State<'_, Arc<TermRuntime>>,
    input: AllowedOperationDto,
) -> Result<EnqueueAckDto, TermErrorDto> {
    state.remote_api.submit(input).await // Host performs command validation/enqueue.
}
```

Do not interpret `EnqueueAckDto` as Worker completion. The Host REST/WSS contract already defines that semantic distinction.

## 2. Internal single-connection ownership

One `TermRuntime` owns: current profile, pinned identity, one `Transport` adapter, one `SessionClient`, pending request map by UUID, timers, cached DTOs and a bounded feed to UI. All connection mutations are serially executed by `ConnectionManager`. `connect` called twice should be deduplicated or cancel the previous in-progress attempt; it should **not** create multiple Host sessions.

Pseudo-trait:

```rust
#[async_trait]
trait ControlTransport {
    async fn connect(&mut self, profile: &HostProfile) -> Result<(), TransportError>;
    async fn send_text(&mut self, json: &str) -> Result<(), TransportError>;
    async fn recv_text(&mut self) -> Result<String, TransportError>;
    async fn close(&mut self);
    fn observed_peer_identity(&self) -> Option<PeerIdentityProof>;
}
```

A common request dispatcher maps request UUID to waiter; server `snapshot` push is independent of a specific request. A disconnected request is not reported as successful simply because it entered the Term's internal outbound queue. Preserve the approved Host `command.accepted` semantics.

One owner + one protocol handler means all WSS/RTC retries, heartbeat, token rotation and incoming frame validation are consistent. A transport switch must use existing session resume/fence if possible; it must not temporarily occupy a second remote slot. `server_epoch` changes invalidate the old session context.

## 3. Pairing and Host key lifecycle

**Approved conditional design ADR-033**: a stable Host Ed25519 identity, verification before sending login credentials; WSS pin to matching SPKI/public identity, RTC signed binding to negotiated DTLS fingerprint. Do not assume certificate checks can be implemented in WebView fetch because WSS lives in **Rust**, where proper verifier controls are available subject to concrete library proof. Do not automatically accept first seen key; display Host fingerprint and obtain explicit user confirmation against Host UI. Windows Credential Manager/DPAPI is appropriate for optional saved credentials; exact persistence implementation can be selected on Windows during development.

Host profile addresses can change (dynamic IP, DDNS) without changing identity. Identity rotation must prompt deliberate re-pair and must never inherit prior credentials automatically to an unknown key. For this friends-only project, review should focus on demonstrable correct pairing and recovery, not enterprise public-service compliance or speculative threat matrices.

## 4. Storage boundaries

| Data | Storage | Lifetime |
|---|---|---|
| Profile ID, nickname, IP/DDNS, TCP port, direct/RTC choice | Versioned Term config file under user app-data | Persistent |
| Custom signaling/STUN/TURN endpoint URLs | Profile config | Persistent, initially empty |
| TURN ephemeral credentials (if user-supplied) | OS credential store when persistent; otherwise Rust memory | User choice/expiry |
| Paired Host public key / fingerprint | Profile config or protected storage | Persistent until re-pair/forget |
| Username | Config or credential metadata (nonsecret) | User choice |
| Password (optional remember) | Windows Credential Manager / DPAPI-protected vault | Persistent if explicitly requested |
| `access_token` | Rust memory only | 900s, refresh before expiry |
| `resume_ticket` | Rust memory only | Session life; rotate on resume |
| `session_id`, `server_epoch`, generation | Rust memory | Session life |
| Snapshot/catalog/log cache | Rust memory / Vue transient state | Clear on switching profile/Host |

Do not write tokens, raw passwords, or credential-containing JSON to Vite logs, Tauri event payloads, WebView localStorage, exception traces or diagnostic exports. This is simply basic functional secret placement from the approved spec; it is not a new protection mechanism for the device.

## 5. Envelope, enum and validation expectations

- Deserialize wire protocol v1, `protocol=1`; request UUID and exact typed `op`/`args` per official JSON Schema. Server push carries `server_epoch` + per-transport generation monotonic `seq`, but they are **not** config CAS.
- Respect max frame 256 KiB, max string 4096, rate 30 commands/s, host log pagination up to 200 lines, function recursion depth and numeric ranges. Most invalid operations rejected locally with user-friendly message, and Host independently validates.
- Store numeric frequencies as upstream **raw** 10..255 and intensity percentages 0..100; display Hz conversion only as a view helper identical to source GUI, not a network schema rewrite.
- Do not use JSON stringify of Rust upstream enums as wire contract; normalized enum values are defined in `docs/15-*` and schemas.
- `current Host selected device` is shared global mutable state. `expected_active_device_mac` if present offers best-effort stale selection check only.

## 6. ICE/TURN relationship to approved API

ICE is a negotiated transport and NAT traversal stack, not another business API. A self-hosted rendezvous gives exchanging SDP/ICE candidates; optional STUN discovers reflexive candidates and TURN relays traffic over reachable public infrastructure. No preset public URLs. Reuse `SessionClient` and same JSON frames on an ordered reliable DataChannel. Signaling/TURN errors are transport errors and must not be turned into `AUTH_FAILED` or application-level session revocation.

The one-slot Host permit applies across all transports. If both routes become available concurrently, `ConnectionManager` elects a single active generation. RTC proof of Host identity cannot be delegated to the signaling service. The exact Rust WebRTC stack remains subject to technical feasibility validation before the corresponding implementation milestone, not a reason to redesign other project modules now.

## 7. Errors and logging

Normalize into stable categories for UI: `DNS`, `TIMEOUT`, `NETWORK`, `TLS_PIN_MISMATCH`, `HOST_UNPAIRED`, `SIGNALING`, `ICE_FAILED`, `TURN_UNAVAILABLE`, `AUTH_FAILED`, `SESSION_BUSY`, `SESSION_REVOKED`, `RESUME_TICKET_INVALID`, `REMOTE_LIMIT_EXCEEDED`, `INVALID_ARGUMENT`, `CONFLICT`, `UNSUPPORTED_PROTOCOL`, `SERVER_ERROR`. Actual v1 server error `code` values take precedence; local `TermErrorDto` can add `origin:network|auth|protocol|ui` without changing wire protocol. Error strings must not leak credentials. On reconnect each state transition has a user-visible explanation and an actionable retry button when appropriate.

## v1.0.2 — rendezvous invitation handling (F-05)

- `Connect → Host profile → ICE/Rendezvous`: add **Invite token** field; Terminal submits it as `join.authorization`. Separate from Host Remote API username/password and Host fingerprint.
- Optional `Remember invite token` stores it through the same Windows Credential Manager / DPAPI mechanism as saved Host password, under a **distinct profile-scoped secret reference**. Plain profile JSON stores only the reference, never the token itself. Default: not remembered; keep in native Rust memory until profile connection ends.
- The owner/operator provisions token by their own rendezvous policy file or operator command, communicates it out-of-band. No cloud user database or built-in registration SaaS.
- Failure to authorize signaling shows `rendezvous_auth_failed` and does not overwrite the Host identity pin or claim a Host login occurred. WSS direct mode remains available with blank rendezvous settings.
