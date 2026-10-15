> **v0.15, 2026-10-15:** пользователь утвердил API v1 пакет P-028–038 → **ADR-028–038 ACCEPTED**. **ADR-033 CONDITIONALLY ACCEPTED** до проверки криптографической реализуемости и ревью. Сводка: [`18-proposal-decisions-v1.md`](18-proposal-decisions-v1.md), запись об утверждении: [`19-api-approval-record.md`](19-api-approval-record.md).

# 05. Decision log — v0.15 (API contract approved, implementation pending)

**Дата:** 2026-10-15 · Архитектура обсуждается; Codex пока не запускаем.

## Принято пользователем

| ID | Решение | Статус |
|---|---|---|
| ADR-001 | Source upstream `egui` 1.3.1, `e076bc2`, fork `Zaknin/ShockingVRC-Term` | ACCEPTED |
| ADR-002 | Не меняем BLE/OSC/Engine/Alarm, waveform/modulation/limits/внутреннюю очередь | ACCEPTED |
| ADR-003 | Host остается существующим egui; отдельный Tauri+Vue Remote Terminal | ACCEPTED |
| ADR-004 | Команды через уже созданный Bridge/Worker, один Host Engine | ACCEPTED |
| ADR-005 | Login/password устанавливаются Host и вводятся/сохраняются Terminal | ACCEPTED |
| ADR-006 | LAN, VPN, прямой WAN; UPnP mapping, manual forwarding fallback | ACCEPTED |
| ADR-007 | HTTPS+WebSocket для direct, логин/пароль, зашифрованная связь | ACCEPTED |
| ADR-008 | GUI FPS limiter: focus 30, background 5, minimized 0 render | ACCEPTED |
| ADR-009 | v1 native-first TLS pinning без обязательного домена | ACCEPTED |
| ADR-010 | Terminal принимает IP и FQDN/DDNS; DDNS updater может работать на роутере | ACCEPTED |
| ADR-011 | STUN не открывает TCP-порт, ICE/TURN — отдельный traversal transport | TECHNICAL FACT |
| ADR-012 | **ICE/STUN/TURN тоже включаем в проект**, не оставляем только optional research | ACCEPTED 2026-10-15 |
| ADR-013 | Сначала архитектура и документы, затем код и Codex | ACCEPTED |
| ADR-014 | **Signaling, STUN, TURN URL и credentials — пустые defaults; никакой public/shared инфраструктуры из коробки** | ACCEPTED 2026-10-15 |
| ADR-015 | **Пользователь указывает собственные signaling/STUN/TURN и может разворачивать сервисы сам** | ACCEPTED 2026-10-15 |
| ADR-016 | Наш частный вариант — отдельные ShockingVRC signaling + coturn на московском сервере с OpenShock; не shipping default | ACCEPTED AS PLANNED DEPLOYMENT |
| ADR-017 | **Не более одной Remote Session на Host**, общая квота для WSS и ICE/TURN; локальный egui работает параллельно | ACCEPTED 2026-10-15 |
| ADR-018 | **Host — единый источник состояния:** локальные и удалённые изменения быстро синхронизируются и отображаются фактически, last processed mutation wins | ACCEPTED 2026-10-15 |
| ADR-019 | **Option A ACCEPTED:** Host-only cap запрещает Remote Terminal записывать `Power Limits min/max` выше заданной границы; фактические Host значения отображаются без clipping. Не распространяется на zones/Intensity/presets/Alarm и не ограничивает физический выход | ACCEPTED 2026-10-15 |
| ADR-020 | **Remote Maximum Strength выключен по умолчанию (`enabled=false`).** Пока выключен, локальный `egui` и Remote Terminal могут менять `Power Limits` в обычном диапазоне upstream `0..200` без нового ограничения. Сохранённое число cap не действует, пока Host не включит переключатель | ACCEPTED 2026-10-15 |
| ADR-021 | **Два независимых числовых remote cap — по одному на A и B**, оба в диапазоне `0..200`. Общий согласованный `enabled=false` по умолчанию; при включении проверяется значение только соответствующего канала. A/B меняются локально независимо | ACCEPTED 2026-10-15 |
| ADR-022 | **Reconnect grace = 30 секунд после обнаружения потери связи**, общий для WSS и ICE/TURN. Слот остаётся RESERVED для прежней сессии; по истечении освобождается. Тайминги heartbeat и обнаружения утверждены отдельно в ADR-023 | ACCEPTED 2026-10-15 |
| ADR-023 | **Heartbeat = 5 секунд, обнаружение потери связи ≈15 секунд без liveness response**, одинаково для WSS и ICE/TURN; после обнаружения начинается отдельный 30-секундный grace по ADR-022. Номинально три пропущенных heartbeat, допускается более раннее обнаружение явного закрытия транспорта. Не менять 50 ms Worker ticker | ACCEPTED 2026-10-15 |
| ADR-024 | **Resume ticket**: login по паролю только в свободный слот; возобновление той же сессии — по отдельному одноразово обновляемому `resume_ticket` и `session_id`; атомарное поколение транспорта `transport_generation` блокирует команды со старого соединения (WSS/RTC) | ACCEPTED 2026-10-15 |
| ADR-025 | **Host revocation**: локальный `Disconnect client` немедленно отзывает текущую сессию и освобождает слот; Terminal прекращает автоматический reconnect/login после kick. Это не бан: новый явный вход с действующим паролем возможен. `Remote Access OFF`, смена пароля, logout и рестарт Host аннулируют соответствующие сессии/билеты | ACCEPTED 2026-10-15 |
| ADR-026 | **Secret placement**: сохранение Host username/password — только по желанию в защищённом хранилище ОС; `resume_ticket` и `access_token` остаются исключительно в памяти Rust Terminal, не в Vue/на диске. После рестарта Terminal нужен новый вход, если прежняя сессия ещё не освобождена — дождаться освобождения слота | ACCEPTED 2026-10-15 |
| ADR-027 | **Lost resume ACK**: разрешён ограниченный идемпотентный повтор `auth.resume` только с теми же `session_id`, `resume_ticket` и `request_id`. Host повторно возвращает результат прежнего успешного resume, не увеличивая `transport_generation` повторно и не выдавая новый билет. Повтор ограничен временным окном/кэшем; не работает после logout/revoke/expiry/password change/Remote OFF/restart | ACCEPTED 2026-10-15 |

## API v1 — newly approved ADR-028–038 (2026-10-15)

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

See also [`18-proposal-decisions-v1.md`](18-proposal-decisions-v1.md). Approval does not mean implementation exists.


## Исторический список предложений из ранних drafts (устаревшие статусы; см. принятые ADR выше)

| ID | Предложение | Причина |
|---|---|---|
| PROP-001 | Одни JSON envelopes для WSS и WebRTC reliable ordered DataChannel | Remote GUI/backend независим от транспорта |
| PROP-002 | Remote crate defines `RemoteBackend` trait, Bridge adapter в src-gui | Исключить Rust dependency cycle |
| PROP-003 | Два вида API: direct HTTP convenience + общие WS/DataChannel messages | DataChannel не является TCP/HTTP |
| PROP-004 | Identity key для связи TLS pin и WebRTC negotiated DTLS fingerprint | Signaling сервер нельзя считать trusted endpoint |
| PROP-005 | Self-hostable rendezvous/signaling + coturn reference deployment; сторонний managed только по явному выбору | CGNAT требует внешней достижимой инфраструктуры, no default service |
| PROP-006 | Host listener TCP `4318` (пример), включение Remote Access — локально | Без изменения core |
| PROP-007 | `Command` enum mapping — отдельный versioned API whitelist | Remote UI полный паритет, без удалённого updater |
| PROP-008 | Один backend session/credential namespace для direct и ICE | Не заставлять пользователей регистрировать второй аккаунт |
| PROP-009 | `command.accepted` означает только подтверждённый enqueue, snapshot отдельно | Нельзя выдавать акт исполнения за ack очереди |
| PROP-010 | **CLOSED by ADR-021:** один общий numeric cap отклонён; два независимых A/B | ACCEPTED ADR supersedes proposal |
| PROP-011 | **CLOSED by ADR-017/022/025:** `SESSION_BUSY`, 30 s grace и Host revoke UX утверждены | CLOSED |
| PROP-012 | **CLOSED: принят вариант A; B не включать** | Не блокировать косвенные изменения силы через другие операции |
| PROP-013 | **CLOSED by ADR-022/023/024:** heartbeat 5 s, detection ~15 s, grace 30 s, ticket resume и generation fencing утверждены | CLOSED |
| PROP-014 | Host `Disconnect client` отзывает сессию, Terminal не делает auto-relogin | **CLOSED by ADR-025** |
| PROP-015 | `auth.resume`, ticket/epoch binding и atomic transport fencing | **CLOSED by ADR-024** |
| PROP-016 | Host kick revokes session, no silent auto-login, no permanent ban | **CLOSED by ADR-025** |
| PROP-017 | Ticket/token in Rust memory, credentials in OS vault opt-in; REST+WSS share session | **CLOSED by ADR-026/029/037** |
| PROP-018 | Loss of rotated-ticket response: bounded idempotent replay of same `auth.resume` request | **CLOSED by ADR-027**; exact replay cache TTL and payload format in wire-contract design |

## Вопросы, требующие решения до кода

1. **Deployment tooling для BYO:** формат конфигурации, reference compose/scripts, TLS для signaling и TURN credential issuance. **Выбор собственного сервера вместо shared default уже утверждён.** Для M1 direct WSS сервисы не нужны.
2. **Remote client sessions:** 1 remote operator, общий `active_device`, 5/15/30 и ticket resume утверждены ADR-017–027; wire API v1 утверждён ADR-028–038. Реализация и E2E ещё не выполнены. 
3. **Host identity:** Ed25519, TLS SPKI pin и signed RTC DTLS binding архитектурно утверждены **условно** (ADR-033). Конкретная реализация требует crypto/library feasibility prototype + security review; если несовместимо, запрашиваем новое архитектурное решение.
4. **Direct HTTP login and WSS in-band login:** поддерживать оба, с общей квотой сессий (ADR-029/037). Остаётся проверить реализацию.
5. **PORT/default bind:** TCP 4318 принят как API default (ADR-036). Детали bind/listener/Host GUI остаются предметом дизайна M1; не активировать Remote Access автоматически.
6. **Встроенный DDNS updater / публичный ACME:** по-прежнему deferred, но FQDN уже работает с v1.
7. **Session wire-details:** access token TTL=900s, bounded resume replay=15s/8 entries, command dedup=120s и frame types утверждены ADR-028–038. Детальная Rust реализация и crypto proof остаются инженерными задачами.
8. **Remote Maximum Strength:** Option A, **default OFF и независимые A/B caps утверждены**. Для первой активации предлагаем требовать явного выбора обоих чисел, не назначая скрытый default; визуальные детали Host UI остаются проектными. См. `11-remote-session-and-strength-cap.md`.

## Release strategy (adopted staged design)

M1 (direct host API), M2 (Tauri), M3 (FPS), M4 (ICE/TURN), M5 (end-to-end release), M6 (Tauri on Host). **M4 входит в целевой проект, не research-only.** Технические детали self-hosted инфраструктуры и цель по дате release обсуждаются позднее.

## Командный scope

Все применимые функции Zones/Channels/Tuning/Modulation/Presets/Alarm/Preferences в Remote API через Bridge как есть. Локальные UI темы, tutorial, self-updater — не устройство и в v1 удалённо не выполняются. Это **утверждённая** граница экспонирования, см. `06-command-matrix.md`.
