# 09. Remote session state machine and examples (DRAFT v0.13)

**Проектная семантика, не реализованный код.** Same message protocol for WSS and WebRTC DataChannel.

## Состояния Terminal

`DISCONNECTED → CONNECTING → VERIFYING_HOST → AUTHENTICATING → SYNCING → ONLINE`. При любом сбое: `ERROR` с причиной и возможностью retry; в `ONLINE` disconnect возвращает в `DISCONNECTED/CONNECTING`. До `VERIFYING_HOST` не передавать credentials.

| Transition | Условие | UI |
|---|---|---|
| CONNECTING→VERIFYING_HOST | Прямой TLS handshake или ICE+DTLS channel создан | Connecting / Authenticating Host |
| VERIFYING_HOST→AUTHENTICATING | Trusted Host identity проверена (TLS pin или signed RTC binding) | Signing in |
| AUTHENTICATING→SYNCING | auth.result success | Loading data |
| SYNCING→ONLINE | Получен full snapshot + capabilities | Connected, live controls |
| ONLINE→DISCONNECTED | Transport lost / Remote OFF / logout | Отключено; отменить *локально неподтверждённые UI-операции*, не откатывать уже отправленные в Bridge команды |

## WSS path

1. TCP + trusted TLS pin check (клиент Rust).
2. WSS upgrade `/api/v1/ws` (или REST login и Authorization bearer header); server accepts `hello`/`auth.login`, не отдаёт private snapshot без авторизации.
3. После auth result → `capabilities` + `snapshot`, затем updates and commands.
4. Remote Access OFF → close sessions/listener, не выключает локальный ShockingVRC.

## ICE/TURN path

1. Both Host and Terminal establish outbound signaling to rendezvous; offer/answer/ICE candidates exchanged, TURN fallback if required.
2. DTLS completes, client verifies negotiated DTLS identity against long-term Host identity (signed binding). **Signaling TLS and room ID alone insufficient.**
3. Reliable ordered DataChannel opens. Same messages `hello`, `auth.login`, `snapshot`, `command` as WSS; no HTTP inside datachannel.
4. TURN relays only encrypted datachannel transport; actual username/password and commands stay end-to-end between Host and Terminal.

## Snapshot vs ACK
**New agreed Host policy (v0.5):** at most one remote operator across WSS and RTC; login by a second independent client → `SESSION_BUSY`, no takeover. On temporary disconnect, preserve slot for **30 seconds after connection loss is detected** (approved, ADR-022; heartbeat 5 s / detection ~15 s accepted as ADR-023; see `12-session-lifecycle-recovery.md`), then expire; logout frees immediately. Host-local egui always works; Host can revoke the remote session.

**Live authority:** local and remote changes converge on one Host `Bridge::snapshot`, including Host-set values exceeding a remote-only power ceiling. Remote Terminal must show unmodified actual Host values and separate Host A/B policy (`remote.policy` with `max_by_channel` proposed). **Option A:** remote writes to `Power Limits min/max` above the Host cap for the targeted channel (ADR-021) are blocked before queue with `REMOTE_LIMIT_EXCEEDED` **only when policy `enabled=true`**. Default OFF allows both interfaces the normal upstream `0..200` range; other commands (including zones, presets, Intensity, Alarm) are not subject to cap enforcement. Do not promise global physical ceiling. An impossible max decrease due to existing min is rejected rather than falsely acknowledged.


Пример:

```text
Host Worker (50 ms) ---> Bridge::snapshot() -> RemoteBackend
Client -> command(request_id=X) -> adapter validates -> Bridge queue
Host -> command.accepted(request_id=X,queued=true) [only after verified enqueue]
Worker -> applies when scheduled
Host -> snapshot(seq=N+1, current state)
```

`Snapshot.revision` растёт по тикеру и не подтверждает «применено именно X». Если команда изменила поле на то же значение, следующий снимок также не докажет отдельного execution. В первой версии интерфейс показывает **queued** и затем состояние по полученному снимку; статус `applied` не обещается.

**Reconnect/revocation ACCEPTED:** volatile ticket bound to original session and Host `server_epoch`, fencing on reconnect, no automatic login after kick (ADR-024–027). Exact wire message fields are still draft.

## Error classes and retry behavior

- `INVALID_ARGUMENT`, `UNSUPPORTED_OPERATION`: не повторять.
- `AUTH_FAILED`, `identity_mismatch`: не отправлять password снова автоматически бесконечно; требовать исправления.
- `UNAVAILABLE`, `transport_closed`, `ice_timeout`: можно переподключаться и после успешного login получать полный snapshot.
- Команды с неясным ACK **не ретранслировать автоматически**, иначе можно повторить операцию.
- Периодические server events на wire не являются источником силового/OSC тайминга, этот цикл в `src-core` прежний.

## Backward compatibility

Protocol v1 clients must validate server protocol range and capabilities. A future `protocol=2` may add independent client-selected device or idempotency but cannot silently change the meaning of v1 `SwitchDevice` and index-based commands.


## Authentication and revocation flow — accepted in v0.13 (wire draft)

Approved proof-of-session, Host-local revoke, secret-storage and fencing behavior is described in `13-session-auth-revocation.md` (ADR-024–027). A key distinction: authenticated password creates a **new session only when FREE**, whereas a volatile ticket resumes **the same** session during ACTIVE/RESERVED. `Logout` releases the slot; `Disconnect client` immediately invalidates it but does not ban a future explicit login with valid credentials. A stale command already queued before revocation may still be processed by the existing Worker; no fresh old-generation commands may enqueue afterward.
