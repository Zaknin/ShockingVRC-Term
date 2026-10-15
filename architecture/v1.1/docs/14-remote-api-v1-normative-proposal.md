# 14. Remote API v1 — approved normative design (v0.15)

**Статус: ACCEPTED (design only) 2026-10-15; ADR-028–038.** Это утверждённый проектный контракт, **не работающая реализация**. При расхождении с историческими черновиками в `02-*` приоритет имеет этот документ и `15-*`/новые JSON Schema. Host identity детали из `17-*` утверждены **условно до crypto feasibility review (ADR-033)**.

## A. Архитектурные инварианты

- Host — исходный `egui` ShockingVRC 1.3.1 (`e076bc2`), один `Bridge`/`Worker`, один выбранный `active_device`; local GUI и remote Tauri показывают **реальное** Host state. Без `src-core`/BLE/OSC/Alarm/queue рефакторинга.
- Transport-agnostic application frames: JSON UTF-8 over **WSS text frames** или **WebRTC ordered/reliable DataChannel**. Один session manager на оба транспорта; максимум **один** remote operator. `ICE/STUN/TURN` входит в scope проекта, BYO endpoints пусты по умолчанию.
- Remote cap A/B (`enabled=false` default) применяется **только** к `channels.set_limit_min/max`, *не* к физическому output, Alarm, zones, presets, Intensity, `config.load`. Проверка на Host до enqueue; remote policy fields read-only.
- Host local `Disconnect`/OFF/revoke/password change немедленно инвалидируют подходящие сессии и секреты, без остановки engine; remote kick не вызывает автоматический login.
- Heartbeat = **5s**, обнаружение обрыва **~15s**, затем RESERVED **30s**. Повтор `auth.resume` с прежними `request_id/session_id/ticket` в коротком окне обязан отвечать идемпотентно.

## B. Endpoints — утверждённый контракт

| Метод | Direct HTTPS path | Назначение | Auth |
|---|---|---|---|
| GET | `/api/v1/health` | Minimal API-alive, только `protocol_min/max`; не раскрывать устройства/IP/policy | — |
| POST | `/api/v1/auth/login` | Альтернативный login для native REST consumer; занимает **тот же один** слот | verified TLS + password |
| POST | `/api/v1/auth/refresh` | Обновить REST bearer **в той же активной сессии**, без login/нового слота | Current valid Bearer session |
| POST | `/api/v1/auth/logout` | Немедленный revoke/logout | Bearer session |
| GET | `/api/v1/snapshot` | Полный typed Host snapshot | Bearer session |
| GET | `/api/v1/capabilities` | Version/features/operations | Bearer session |
| GET | `/api/v1/catalogs/presets` | Presets (включая editable local/user flags), порядок | Bearer session |
| GET | `/api/v1/catalogs/zone-presets` | Zone-presets, порядок | Bearer session |
| GET | `/api/v1/logs?cursor=&limit=` | Sanitized 1–200 lines | Bearer session |
| POST | `/api/v1/commands` | Та же whitelist команда/ответ через общее policy/dispatch | Bearer session |
| GET upgrade | `/api/v1/ws` | WSS; canonical interactive login/resume/streams | WSS auth frame или Bearer attach |

`/auth/login` по HTTPS — второй способ создать ту же сессию, **не второй пул клиентов**. После HTTP-login Host резервирует слот как `PENDING_ATTACH` **на 15 секунд** (ADR-037), ожидает WSS attach или HTTP heartbeat; при отсутствии активности слот освобождается. REST-only клиент поддерживает `/api/v1/session/heartbeat` (POST authenticated), иначе active session истечёт по тем же 5/15/30 правилам. Для непрерывной REST-only работы клиент **до истечения 900s TTL** вызывает `POST /api/v1/auth/refresh` с текущим Bearer, получает `{access_token,access_expires_in}` и продолжает heartbeat без создания нового slot. После истечения токена обновление по этому endpoint невозможно; применяются обычные session/resume правила. Для простоты реализации на первом этапе Tauri использует **WSS in-band login**, HTTP login/commands допускаются лишь при согласованной полной реализации (не фальшивые endpoint-заглушки). Все endpoint errors возвращают общий JSON `code`, `message`, `request_id?`; заголовки `Cache-Control: no-store`. Никогда не передавать credentials/tickets в URL/query.

Для v1 утверждён **полный REST companion** из таблицы (ADR-029/037); это увеличивает test surface, но даёт REST-интеграции без второго API. Приложение Tauri не вызывает REST login, если уже вошло через WSS.

## C. Frame envelope v1

Каждый пакет — **один JSON object**; `protocol:1`, `type:string`, `request_id:UUID` на client-initiated request и reply. Server pushes используют `server_epoch:UUID`, `seq:uint64` в рамках текущего соединения, но `seq` **не** global revision. Переменные размерности: max frame 256 KiB, max string 4096 chars, max burst 30 commands/s на одну сессию; данные логов передаются страницами (параметры DRAFT). Reject duplicate JSON keys, NaN/Infinity, неизвестный `op`, чужие privileged fields, invalid UTF-8, malformed JSON, excessive depth, extra commands. Игнорировать новые серверные read-only fields допускается только в version-compatible ответах; входящие мутации валидируются строго.

**Request/response types:**

| Request | Reply | Назначение |
|---|---|---|
| `hello` | `hello.ok` | version/identity handshake; без приватного state |
| `auth.login` | `auth.result` / `error` | новый remote slot |
| `auth.resume` | `auth.result` / `error` | возвращение прежнего session_id, ticket rotation |
| `auth.attach` | `auth.result` / `error` | WSS attach к действующей REST/Bearer-сессии; нового слота нет |
| `auth.token.refresh` | `auth.token` / `error` | ротация access token в активной сессии |
| `auth.logout` | `auth.logged_out` | отзыв + release |
| `state.get` | `snapshot` | full resync |
| `capabilities.get` | `capabilities` | supported ops and features |
| `catalogs.get` | `catalogs` | preset catalogs; lazy load |
| `logs.get` | `logs` | log cursor & page |
| `command` | `command.accepted` / `command.error` | validated send to **existing** Bridge |
| `ping` | `pong` | liveness, every 5 s |
| server push | `snapshot`, `remote.policy`, `server.notice`, `session.revoked`, `logs.append`, `catalogs.changed` | state changes/events |

`hello` carries 32-byte client nonce encoded base64url, app version, transport identifier. `hello.ok` returns stable Host identity public key, fingerprint, `server_id`, fresh `server_epoch`, 32-byte server nonce and binding proof. `auth.*` is **not accepted** before Host identity verified by Terminal (see `17-host-identity-and-transports.md`). The wire cannot force honest remote clients to verify TLS, but reference Terminal must refuse untrusted Hosts.

Transport WSS text and RTC messages share the identical JSON content and `command` op semantics; WebRTC DataChannel DOES NOT pretend to expose HTTP methods. Server must not process frames after logout/kick/expired/fenced generation.

## D. Credential / session contract

- `auth.login`: `username`, `password`; Host Argon2id hash + random salt; uniform auth error without revealing whether username exists. `AUTH_FAILED` and rate-limit backoff. Host remote access defaults OFF.
- On successful login: `session_id:UUID`, `resume_ticket` (CSPRNG ≥256 bit, base64url), `access_token` (independent 256-bit opaque bearer), `access_expires_in:900` seconds, `transport_generation:1`, `server_epoch`, `session_state:"ACTIVE"`.
- `access_token` is used only for REST and optional WS Bearer attach. Rotate before TTL expiry by `auth.token.refresh` **over authenticated live WSS/RTC of current generation**, or by **`POST /api/v1/auth/refresh`** with the current valid Bearer for a REST-only client. Both methods renew the existing session token only, never acquire a second session slot; do not let renewal create another slot, store previous verifier only for brief overlapping HTTP in-flight requests (design target 10 s). It cannot independently resume a lost connection; only resume_ticket can. Ticket remains in Host/Tauri Rust process memory. Never in Vue, localStorage, URL, stderr or exported logs. Host may persist password hash, Host identity, and remote cap; NOT live tickets across restart.
- `auth.resume`: `{session_id,resume_ticket,request_id,client_nonce}` and proven Host identity; allowed while ACTIVE or RESERVED only for same client. Old transport generation is fenced **before** new command admission. Response rotates ticket/access token and increments generation ONCE. Client always receives full snapshot after resume.
- Lost ACK: cache exact result keyed by `(server_epoch, session_id, request_id, HMAC(ticket))`, for **15 s** (ADR-030), up to **8 entries per session**. Exact identical replay responds with identical result; different request_id + consumed ticket => `RESUME_TICKET_INVALID`. No second generation. Revoke/expiry/restart clear cache. Retry schedule 1s / 2s / 4s, max 3 (ADR-030), within 30s grace.
- Explicit Host kick pushes `session.revoked {reason:"host_kick"}` on active connection if possible. Terminal marks `REVOKED`, clears tokens, STOPS auto-login and auto-resume. A new **explicit** user click may login with same password. Change password/Host OFF/restart also invalidate; never emulate revoke by clearing only a browser-side flag.
- Host process restart = new `server_epoch`, invalidates all sessions/tickets; saved Host public identity remains if intentional install; client must login after validating pin.
- Liveness uses authenticated application-level `ping`/`pong`, not only OS TCP keepalive. Silence ~15 s → RESERVED; 30 s after detection → FREE. A user-initiated Logout frees immediately. Network disconnect alone never powers off a channel or changes alarm.

## E. Command delivery semantics (critical)

1. Parse strict `command {op,args,request_id}`; apply canonical authorization, generation fencing, selected-target checks and **Host remote A/B cap** on the common adapter before existing Bridge.
2. After exact validation, submit the matching unchanged `Command` to the existing `UnboundedSender` and ensure send success (`Result`). Existing `Bridge::send` currently returns `()`; a *small additive* send-result method may be needed **without changing Worker/queue behavior**. If not implemented, reply must be `command.received`, NOT fabricated `command.accepted:queued=true`.
3. On proven enqueue, reply `command.accepted {queued:true}`. **This is not an apply ACK, persistence ACK, or hardware ACK.** Current value visible in later `snapshot`. The *last mutation processed by Worker* wins; no client-side optimistic state should override authoritative snapshot.
4. Request dedup: remote terminal must not blindly replay unacknowledged mutating commands after reconnect. Host may cache accepted command `request_id+payload_digest` within a session for 120 s (ADR-030) and respond with cached ACK, but cannot retrospectively prove downstream effects. Different payload with same ID => `DUPLICATE_REQUEST_CONFLICT`.
5. `Snapshot.revision` increments on every ~50-ms publish. No config-CAS based on it. Bounded 5–10 Hz network snapshot updates, full on login/reconnect; `seq` monotonically increases per transport generation. On gap/reconnect, request `state.get`; no silent extrapolation of missed states.
6. CLI `active_device` is shared. API can precheck optional `expected_active_device_mac` but existing Worker doesn't perform atomic check at apply time; **do not promise atomic target selection**. Avoid multiplying underlying Worker's mutable device context. Indexed zone/preset commands likewise can drift; client must resync lists after changes. If strict atomic targeting becomes required, request a separate ADR; do not quietly redesign Worker.

### Remote cap exact rule

When `remote_strength.enabled=true`, for `channels.set_limit_min/max` reject requested `value` greater than `max_by_channel[channel]` with `REMOTE_LIMIT_EXCEEDED`. Before enqueue, also reject a max request which original `PowerLimits::new` would normalize over requested max because current `limits.min` > requested value. Host values above the cap are transmitted **unchanged**, with remote policy beside them. `enabled=false` uses original PowerLimits 0..200 on both channels. `config.load`, presets, alarm and zone changes never route through cap check. Host-only cap settings are **not remote mutating ops**.

## F. Required operator experience

- `CONNECTING` → `VERIFY_IDENTITY` (first pairing/changed pin requires explicit Host-verified fingerprint) → `AUTHENTICATING` → `SYNCHRONIZING` → `ONLINE`.
- `DEGRADED` when no freshness; sliders locked while disconnected/resuming. `RECONNECTING` up to accepted 30s reservation; fresh full Snapshot required before controls re-enabled. `SESSION_BUSY` displays busy status, **never** steals session.
- Host GUI can view remote client display name/session connected since, transport (direct / relay), last heartbeat, and `Disconnect client`/Remote OFF. No public exposure of username/password/ticket/client network address in anonymous health.
- `REMOTE_LIMIT_EXCEEDED` shows A/B-specific allowed max and actual host value separately, without clipping.
- Log pages and historical events sanitized of tokens/passwords. `server.notice` supports informational operational events; not a source of truth for actuator output.

## G. Compatibility and backward behavior

- v1 wire JSON enumerations fixed separately from Rust serde defaults. Unknown **request** type/op => `UNSUPPORTED_OPERATION`; unsupported protocol version => `UNSUPPORTED_PROTOCOL` before login. Unknown response data fields may be ignored only within v1-compatible major.
- Version capability list declares optional `rtc_datachannel`, `rest_commands`, `logs.stream`, `remote_strength_cap`, `session.resume` independently. Never advertise until implemented.
- Existing 137 upstream tests must continue to pass. M1 may ship **direct WSS host** before ICE/TURN but final project includes ICE/TURN and independent self-host service. No pre-configured rendezvous/STUN/TURN or hardcoded Moscow endpoint. No code until architecture accepted.

## V1.0.2 — precision corrections, owner-approved 2026-10-15

- F-01: REST bearer renewal via `POST /api/v1/auth/refresh`; `200` returns the same fields as `auth.token` **without WSS envelope** (`access_token`, `access_expires_in`). A current valid bearer is required; refresh is authorized only for the same ACTIVE session, keeps current session_id, restarts 900s TTL, does not grant a second login or reset the one-client policy. REST heartbeat remains independent.
- F-02: `state.prefs` is a strictly typed optional snapshot object; Host publishes it if its prefs adapter supports both fields. `capabilities.features` includes `snapshot.prefs` **only when** the field is supplied; otherwise Term leaves these Setup controls unavailable rather than inventing current values.
- F-03: interactive `auth.result.session_state` is mandatory constant `ACTIVE` in success frames.
- F-06: `GET /capabilities`, `/catalogs/*`, `/logs` have typed JSON bodies; `POST /commands` gives **202 only for proven `command.accepted`**. If enqueue proof cannot be supplied but the request was received, **200 `command.received`** is the documented fallback, explicitly not an enqueue or apply ACK. All HTTP payloads omit WSS `protocol/type/request_id` wrapper unless explicitly specified by their REST DTOs; original WSS/DataChannel wire frames unchanged beyond the F-03 field.
- These edits correct the already approved v1 contract; they do not introduce new engine behaviors, remote commands, security programs or test requirements.
