# 08. Test plan / API contract acceptance (без hardware writes)

**Status: Draft v0.12**

## Условия

Base `egui` @ `e076bc2`, 137/137 local `cargo test --workspace` PASS согласно предоставленному логу. Новая функциональность не должна изменять `src-core/`, параметры, алгоритмы и FIFO Worker. На CI и в первых тестах использовать `Bridge::detached`, fixture Snapshot, fake catalogs и fake router/transport; не подключать физическое устройство.

## Матрица проверок

| ID | Проверка | Наблюдаемый ожидаемый результат |
|---|---|---|
| API-01 | WSS/RTC hello protocol=1 | верный `hello.ok` с server_id/protocol, **нет приватного Snapshot** до login |
| API-02 | Неверный логин или пароль | один внешний `AUTH_FAILED`, секреты отсутствуют в логах |
| API-03 | Valid login | full snapshot + capabilities |
| API-04 | logout/change password/remote OFF | сессии закрыты / отозваны, локальный Worker продолжает работу |
| API-05 | `channels.set_frequency` mock | ровно один существующий `SetFrequency` в receiver detached Bridge; никаких core writes |
| API-06 | Каждая команда `06-command-matrix.md` | valid payload maps to exact upstream Command variant; invalid payload rejected |
| API-07 | segment -1,4; enum wrong; overlarge number | validation fails; на канале Bridge нет команды |
| API-08 | index устарел / каталог изменился | задокументированное поведение, ошибки при известных случаях; **не заявлять атомарную проверку** |
| API-09 | snapshots from shared bridge | deep DTO completeness, prescribed fields/serialized values |
| API-10 | rapid snapshots | ограничивается *сетевая публикация*, Worker 50мс неизменён |
| API-11 | WS disconnected mid-command | после reconnect full snapshot; нет автоматического повторения неподтверждённых command |
| API-12 | read-only logs | нет секретов; учитываются limit/cursor; нет возможности записать лог через API |
| API-13 | host and remote GUI simultaneous | оба видят один `active_device`, same config, documented last-processed command wins |
| API-14 | remote host restart | server_epoch изменён, full re-auth/resnapshot, сохранённая Host identity проходит |
| API-15 | две remote попытки (WSS + RTC), локальный egui работает | только одна активная удалённая управляющая сессия; второй login `SESSION_BUSY`, исходная сессия не вытесняется |
| API-16 | local changes vs remote changes | оба интерфейса показывают текущую конфигурацию Host; actual `active_device`, включая пересечение команд |
| SESSION-01 (ACCEPTED TIMINGS) | Authenticated liveness every 5s (ADR-023); ~15s without response triggers loss detection (ADR-023); 30s reservation **from detection** (ADR-022) | Three missed heartbeats nominally lead to RESERVED, then expiry after 30s; explicit socket close can detect sooner; unchanged Worker ticker |
| SESSION-01b (ACCEPTED TIMINGS) | Healthy connection continues to answer the 5s heartbeat | No false expiry; ACTIVE remains ACTIVE |
| SESSION-01c (ACCEPTED TIMINGS) | Three missed heartbeats trigger RESERVED; second client tries to login before expiry | `SESSION_BUSY` during the 30s reservation; slot free after grace |
| SESSION-02 (ACCEPTED DESIGN) | Valid ticket resume while old socket alive | Old generation fenced; only new transport can submit command |
| SESSION-03 (ACCEPTED DESIGN) | Correct password from second terminal | `SESSION_BUSY`, no takeover even during reservation |
| SESSION-04 (ACCEPTED DESIGN) | Host revoke and Remote OFF | Ticket invalid, remote control ends, no Terminal auto-relogin after kick; Host engine unaffected |
| SESSION-05 (ACCEPTED DESIGN) | WSS→RTC resume with configured BYO infrastructure | Same slot, verified Host, fresh Snapshot, no stale packet acceptance |
| SESSION-06 (ACCEPTED DESIGN) | Host restart, unexpected Terminal death, lost ACK | Correct fresh auth/grace expiry; no automatic mutation replay |
| AUTH-00 | Fresh Host install; no remote cap enabled; remote sets `A.max=200`, local sets `B.max=200` | No cap-based rejection; upstream `0..200` behavior applies to both |
| AUTH-00b | Stored A cap=70 and B cap=120, `enabled=false`, remote writes `A.max=180`,`B.max=200` | Both accepted; stored values inert while policy disabled |
| AUTH-00c | Remote active; Host toggles disabled→enabled with A cap=70,B cap=120→disabled | Remote sees policy updates; A=71 rejected only while enabled, accepted again when disabled |
| AUTH-01 | Host A cap=70,B cap=120, remote `A.set_limit_max=71` | `REMOTE_LIMIT_EXCEEDED` (`channel=A`,`allowed_max=70`), nothing enqueued; local changes permitted |
| AUTH-02 | Host sets A.max=150 and B.max=180 with A cap=70,B cap=120 | Remote sees true A.max=150 and B.max=180 with independent caps, does not clamp |
| AUTH-03 | Remote tries above the selected channel cap via REST/WSS/RTC | Same check applies at common Host remote adapter; no bypass |
| AUTH-04 | Remote zone/Intensity/modulation/preset/Alarm action with A=70/B=120 policy | No new cap-based block, upstream semantics unchanged |
| AUTH-05 | A cap=70, remote `A.set_limit_min=71` | `REMOTE_LIMIT_EXCEEDED`; no enqueue |
| AUTH-06 | Host A.min=80,max=120; A cap=70; remote A.max=70 | Reject due to upstream normalization; state unchanged |
| AUTH-07 | A cap=70, remote A.max=70, Host A.min<=70 | Enqueue permitted; snapshot confirms result |
| AUTH-08 | Host A cap=70,B cap=120; remote B.max=100 | Accepted under normal validation; A cap must not constrain B |
| AUTH-09 | Host A cap=70,B cap=120; remote B.max=121 | `REMOTE_LIMIT_EXCEEDED` (`channel=B`, `allowed_max=120`); no enqueue |
| AUTH-10 | Host updates A cap=90 leaving B=120 during active connection | Event updates A only; B remains 120; no reconnect |
| AUTH-11 | First-run Host has no A/B values; tries to enable master cap | Cannot enable until both explicitly entered, no silent numeric defaults |
| SEC-01 | Man-in-middle wrong pinned TLS | отказ ДО логина |
| SEC-02 | WebRTC DTLS fingerprint tampered in signaling | authenticated binding fails ДО логина |
| SEC-03 | token in URL/log | не появляется в URL, access logs, UI telemetry |
| NET-01 | LAN/VPN direct TCP | HTTPS/WSS full API OK |
| NET-02 | UPnP works/refuses/times out | status корректен, LAN продолжает работать |
| NET-03 | DDNS IP change | reconnect with same pinned host identity when same host |
| ICE-01 | Direct ICE candidate pair | protocol frames equivalent to WSS |
| ICE-02 | No direct path → TURN | relay datachannel, auth, snapshot, commands, read-back |
| ICE-03 | TURN missing/invalid creds | clear failure, direct/manual/VPN still available |
| ICE-04 | signaling unavailable | diagnosed; no false connected status |
| ICE-05 | RTC transport dropped | new session/full snapshot; no queued command replay |
| ICE-06 | Fresh install default network settings | rendezvous null, STUN [], TURN [], no outbound requests to public endpoints |
| ICE-07 | User-defined infrastructure A and B | each client connects only configured server; profiles independent; no hidden fallback |
| ICE-08 | Moscow private test deployment | separate rendezvous/coturn co-hosted with OpenShock, no hard-coded address in binary |
| REG-01 | upstream 137 tests | 137/137 unchanged PASS (new tests extra) |
| REG-02 | `git diff e076bc2 -- src-core/` | empty, если отдельного одобрения на изменения не было |
| UI-01 | active 30, inactive 5, minimized 0 target | измеряем actual render FPS и CPU/GPU; не путать с 50-мс Worker |

## Набор deliverables

- Unit tests DTO validation/mappers/auth/identity; integration tests for both WSS and RTC transport with mock Bridge; fixtures to compare wire DTO completeness to upstream structures.
- Network simulations: fake UPnP/PCP gateway, signaling exchange, two private NAT networks and public TURN (integration lab only).
- Manual lab: LAN, VPN, WAN using real home router of tester and separately off-LAN terminal; no claim of working WAN based on hairpin test.
- Security review of login/TLS/DTLS identity/signaling paths; no test asserts modifications of physical intensity semantics.
- Source commit, logs, generated schemas, test outcomes, manifest and hashes as evidence, including failures. Publish first to `\\\<evidence-share>\Torrents\Codex\ShockingVRC-Term\<stage>\...`, readback+SHA-256, `.sha256` last. Do not claim publication without verification.

## Критерий перехода к Codex

**API v1 design APPROVED** by user 2026-10-15 (ADR-028–038), one-session/5-15-30 timings and Option A A/B caps approved earlier. **Engineering gates remain:** Host server architecture, crypto identity binding feasibility/security review (ADR-033), planned M4 BYO rendezvous/coturn deployment, and contract/integration tests. Approval alone is not readiness to expose a network listener or control real hardware.


## Auth and revocation scenarios — accepted behavior v0.13 (wire draft)

The `AUTH-01`–`AUTH-14` test matrix in `13-session-auth-revocation.md` augments the current contract tests with Host pin-before-credentials, separated password/token/ticket roles, no password-based takeover, true single-slot REST+WSS binding, fenced old generation, local Host kick and re-entry, cross-transport resume, and lost-ACK recovery. These tests verify accepted behavior (ADR-024–027) and **must be mapped to the future wire format** before coding. They are not yet implemented or executed.


## ADR-024–027 acceptance tests (approved 2026-10-15; pending implementation)

- Resume with identical `session_id`, `resume_ticket`, `request_id` after lost ACK: exactly one rotation and generation increment; retry returns same resulting ticket/generation within bounded window.
- Same `session_id` and old ticket with a *different* `request_id` must not reuse consumed ticket to create another generation.
- Expired retry window or Host logout/kick/Remote OFF/password change/restart invalidates replay cache as well as old ticket.
- Commands from old transport generation arriving after resume/revoke are rejected **before** `Bridge::send()`.
- Credentials opted into OS credential vault persist, while tickets/tokens do not persist across Terminal process restarts or reach the WebView.
- Second Terminal with correct password cannot seize ACTIVE or RESERVED session without the valid resume ticket.
- No automatic replay of unacknowledged mutation commands, distinct from permitted duplicate `auth.resume` retry.
