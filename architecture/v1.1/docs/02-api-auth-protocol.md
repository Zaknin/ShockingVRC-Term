> **v0.15 status:** этот документ — исторический проект. Утверждённая норма Remote API v1: [`14-remote-api-v1-normative-proposal.md`](14-remote-api-v1-normative-proposal.md), [`15-data-model-and-operation-types.md`](15-data-model-and-operation-types.md), ADR-028–038. При расхождениях применять утверждённые документы.

# 02. Remote API v1 — согласуемый контракт

**Status: Proposed for review · 2026-10-15 · Draft v0.13. Не реализовано.**

## Назначение и неизменяемые границы

Одна управляющая реализация на Host — `src-gui/src/bridge.rs`. Сетевой интерфейс **не** вызывает `CliEngine`, BLE, OSC и Alarm напрямую. Он читает `Bridge::snapshot()` / `Shared` и переводит удалённые команды в существующие `bridge::Command` без изменения их бизнес-семантики. В `src-core/` изменений не предполагается.

Один экземпляр Host использует один `Bridge`/`Worker`; локальный egui и удалённый терминал видят общее текущее устройство. На первом этапе сохраняем `active_device: usize` из Worker, удалённых независимых device-context не вводим. Клиент не должен утверждать, что команда применяется к ранее выбранному устройству, если локальный оператор успел переключить `active_device`.

**Ключевое архитектурное правило:** транспорт сменный. Один versioned JSON message protocol использует `WSS` в прямом режиме и *reliable, ordered WebRTC DataChannel* в ICE/TURN режиме. REST служит дополнительным интерфейсом direct HTTPS; WebRTC не обязан воспроизводить HTTP поверх data channel. Все команды и состояния должны существовать в transport-neutral application layer.

## Интеграция с существующим Bridge: отсутствие циклических зависимостей

`src-gui` уже содержит Bridge. Если новый `src-remote` будет импортировать `shocking_vrc_gui`, а GUI будет импортировать remote для запуска сервера, получится Rust dependency cycle. **Не допускать.**

Предлагаемая граница:

- Новый `src-remote`: транспорт, API/DTO, аутентификация, session manager, сигнальный и traversal adapter; определяет `RemoteBackend` trait (`get_snapshot`, `get_catalogs`, `read_logs`, `submit_command`) и не зависит от `src-gui`.
- `src-gui`: адаптер `BridgeRemoteBackend`, преобразующий DTO в существующий `bridge::Command`, использующий текущий `Bridge`, `Shared`, `LogBuffer`; создаёт один `RemoteServer` из composition root (`main.rs`/GUI lifecycle).
- `src-core`/`src-app`: остаются как есть; если выяснится неизбежная причина менять их, сначала отдельное согласование.
- `src-terminal` (позже): Tauri Rust networking + Vue; **не** собирает Engine/BLE/OSC.

Такой layout — предложение до Coding ADR, а не утверждение конкретных имён Rust методов.

## Версионирование

- Базовая версия wire protocol: `1`; у всех сообщений `type` и `protocol`.
- `server_id` — устойчивый идентификатор Host (не IP/DDNS), `server_epoch` — случайный ID конкретного запуска сервера. `seq` — порядок отправленных server events **в этой сессии**.
- `Snapshot.revision` в upstream **увеличивается каждые 50 мс**, даже когда config не менялся: использовать лишь для отслеживания обновлений, **не как версию конфигурации/CAS**.
- `capabilities` — реальный перечень доступных команд и версий функций; неподдерживаемый `op` получает `UNSUPPORTED_OPERATION`.
- Имена команд и enum wire values стабильны внутри protocol v1; Rust enum/serde layout не экспортируется на wire автоматически.

## Transport-neutral сообщения

**Клиент → Host**: `hello`, `auth.login`, `state.get`, `capabilities.get`, `catalogs.get`, `logs.get`, `command`, `ping`, `auth.logout`.

**Host → клиент**: `hello.ok`, `auth.result`, `snapshot`, `capabilities`, `catalogs`, `logs`, `command.accepted`, `command.error`, `server.notice`, `pong`.

Сначала Host выдаёт `hello.ok` с protocol/version/server identity и **без приватных данных**. После проверки Host identity клиент передаёт `auth.login`; до авторизации разрешены только hello/login/ping и ограниченные сообщения об ошибке. После `auth.result(ok=true)` сервер сразу передаёт полный `snapshot` и `capabilities`. Обновления Snapshot/телеметрии — по изменению с верхним целевым темпом 5–10 Гц; это ограничение *сетевой доставки*, не изменение 50-мс Worker.

Пример входа:

```json
{"type":"hello","protocol":1,"request_id":"b78bdb30-529b-488e-9061-6d08aaf1f021","client":"ShockingVRC Remote","client_version":"0.1.0"}
```

```json
{"type":"auth.login","protocol":1,"request_id":"49d8b394-c7e6-4aa8-a295-bc04a44cc46c","username":"nightmare","password":"<entered locally; never logged>"}
```

Пример команды (отражает `Command::SetFrequency(Channel::A, 0, 40)`):

```json
{"type":"command","protocol":1,"request_id":"db63ef57-044a-4ea1-ad43-c56fb0c31c38","op":"channels.set_frequency","args":{"channel":"A","segment":0,"value":40}}
```

Ответ после проверки протокола/полей и передачи в Bridge:

```json
{"type":"command.accepted","protocol":1,"request_id":"db63ef57-044a-4ea1-ad43-c56fb0c31c38","queued":true}
```

**`queued=true` не гарантирует применения Worker, записи на диск и аппаратного результата.** Сегодня `Bridge::send()` возвращает `()` и лишь логирует ошибку закрытого канала. Чтобы честно подтверждать enqueue, адаптеру может потребоваться дополнительная необязательная `try_send`-обёртка к тому же `tx`, **без изменения очереди и Worker**. До неё допустимо отвечать `command.received` (получено API), но **не** `queued=true` как доказанный факт.

Состояние приходят отдельно:

```json
{"type":"snapshot","protocol":1,"server_epoch":"e48a9f1c","seq":203,"state":{"bridge_revision":491,"active_device":0,"devices":[],"vrchat":{"connected":false,"osc_port":33776,"oscquery_port":33776},"config":{"channel_a":{},"channel_b":{},"ukf":{},"norms":{}},"status":{"channel_a":{},"channel_b":{},"device_connected":false},"alarm":{"config":{},"status":{}},"avatar_zones":[]}}
```

Объекты `config`, `status`, `alarm`, `avatar_zones` здесь **сокращены для иллюстрации**. Их полный wire DTO должен покрывать поля текущих `CliConfig`, `CliStatus`, `AlarmConfig`, `AlarmStatus`, `ZoneEvent`, включая значения модулей, массивы сегментов и зону. Пакетные тесты проверяют сохранность всей информации. DTO `PresetState`, `ZonePresetState` и LogBuffer идут отдельными `catalogs`/`logs`, а не ошибочно объявляются частью `Snapshot`.

Ошибка команды:

```json
{"type":"command.error","protocol":1,"request_id":"db63ef57-044a-4ea1-ad43-c56fb0c31c38","code":"INVALID_ARGUMENT","message":"segment must be 0..3"}
```

Стандартные коды: `AUTH_REQUIRED`, `AUTH_FAILED`, `SESSION_BUSY`, `REMOTE_LIMIT_EXCEEDED`, `UNSUPPORTED_PROTOCOL`, `UNSUPPORTED_OPERATION`, `INVALID_ARGUMENT`, `UNAVAILABLE`, `STALE_SELECTION`, `INTERNAL_ERROR`. `STALE_SELECTION` может быть выдан **только** если API реально проверяет ожидаемый контекст; текущий Worker не обеспечивает атомарное условное применение.

## Команды и материалы

Авторитетный реестр команд — `src-gui/src/bridge.rs:Command`. Полный список маппинга, параметры и исключения: [`06-command-matrix.md`](06-command-matrix.md). Включены Zones, Channels, Tuning, Modulation, Presets, Alarm и применимые настройки. UI-only функции темы, просмотр tutorial, локальный updater и локальное управление окном в Remote API v1 не входят. `StartUpdateDownload` и остальные upstream update-команды **не** пробрасываем.

Индексы сегментов — строго `0..3`, поля канала — `A|B`; численные значения на wire до валидации принимать как целые соответствующего JSON-диапазона, не полагаться на автоматическое переполнение `u8`. При переводе в Bridge используем прежнюю логику clamp/sanitise; API не устанавливает новые физические ограничения. Для действий с индексом зоны/пресета учитываем сессионный снимок каталога и документируем смену порядка/содержимого при конкурирующих действиях.

## HTTP endpoint в режиме прямого HTTPS

| Метод | URL | Содержание | Кто может вызывать |
|---|---|---|---|
| GET | `/api/v1/health` | minimal alive + protocol range, без user data | anonymous |
| POST | `/api/v1/auth/login` | username/password → opaque access token | anonymous over TLS |
| POST | `/api/v1/auth/logout` | revoke current session | authenticated |
| GET | `/api/v1/snapshot` | full state DTO | authenticated |
| GET | `/api/v1/capabilities` | features, command names, schema version | authenticated |
| GET | `/api/v1/catalogs/presets` | channel presets + user flags | authenticated |
| GET | `/api/v1/catalogs/zone-presets` | zone preset entries | authenticated |
| GET | `/api/v1/logs?cursor=&limit=` | diagnostic log lines (read only) | authenticated |
| GET | `/api/v1/ws` | WSS upgrade for same message protocol | authenticated Bearer **or** in-band `auth.login` before any state |

`POST /auth/login` и in-band `auth.login` — две оболочки одного backend-auth, с одинаковой политикой. Native Tauri Rust может передать `Authorization: Bearer` в WSS handshake; токены не передавать в URL/query или WebView storage. В режиме WebRTC DataChannel используются только transport-neutral messages, и логин всегда выполняется **после проверки Host identity**.

## Auth и секреты

- Host GUI создаёт один логин/пароль; по умолчанию Remote Access выключен. Пароль: Argon2id + уникальная соль, только hash в app-data, права файлов текущего Windows пользователя. Смена password/выключение Remote Access инвалидирует сессии.
- Session token — криптографически случайный opaque, проверяется сервером; TTL = 15 минут (ADR-030), refresh в рамках живой сессии. На первом этапе можно повторно входить из защищённого локального хранилища Terminal, без долгоживущего refresh token.
- Секрет Terminal через Windows Credential Manager / DPAPI (или OS keychain), не Vue/localStorage/config JSON. Client Rust хранит в памяти только на время сессии, не пишет пароль в logs/telemetry.
- Авторизация до любых приватных данных; ограничение попыток логина и однообразные ответы ошибок — свойства сетевого доступа, не изменение механики шокера.
- Терминал сохраняет пин **Host identity**, привязанный к `server_id`, не к IP или DDNS hostname. Первый pin сверяется с Host по доверенному каналу. Ни WSS, ни RTC не отправляют пароль непроверенному Host.

**Session lifecycle extension (BEHAVIOR ACCEPTED ADR-024–027, wire draft):** `auth.resume` with short-lived session-specific ticket, single-session reservation (**30 seconds after detected connection loss accepted, ADR-022**), atomic fencing of old transport, Host revoke and full resnapshot. These are accepted behavior requirements; the exact v1 message schemas remain to be finalized. See `12-session-lifecycle-recovery.md`.

## Переподключение и потери

- Новый transport session → проверка Host identity → login → full `snapshot` и `capabilities` → live updates.
- После WSS/RTC-disconnect UI показывает Offline; команда без подтверждения **не воспроизводится автоматически**. Уже поставленные в Bridge команды не отменяются самим transport.
- Механизм синхронизации не обещает exactly-once. `request_id` нужен для корреляции ответов, а не для гарантий аппаратного применения.
- Если direct WSS недоступен, Terminal может попытаться ICE, **только если явно настроен пользовательский signaling/rendezvous**. STUN и TURN используют только заданные пользователем endpoints; TURN необязателен для попытки ICE P2P, но нужен для relay fallback. При пустой инфраструктурной конфигурации traversal отключён и **никаких скрытых общих серверов или fallback URL нет**. Выбранный transport не влияет на команды.
- Logs — read-only; remote client не получает серверные ключи, пароль/hash/token, полный путь к закрытым ключам и секретам интеграций.

## Конкурентность и текущий `active_device`
**Принято 2026-10-15:** одна активная авторизованная управляющая сессия Remote Terminal на Host *суммарно* для direct WSS и ICE/RTC. Local egui независим от слота. Другая remote попытка получает `SESSION_BUSY`; reconnect grace 30 s **accepted** (ADR-022; starts after loss detection), local revoke возможен. Конфигурация Host — единственный источник правды, изменения отображаются быстро с обеих сторон.

**Host-only Remote Maximum Strength:** новый remote policy (read-only для терминала) отдельно от `CliConfig`; Host выставляет **два независимых предела A и B**, а клиент видит истинные `limits.max` даже выше соответствующих пределов. Remote API должен отклонять запрещённые запросы кодом `REMOTE_LIMIT_EXCEEDED`; это не просто ограничение элемента UI. **Option A принят:** сервер проверяет только удалённые `channels.set_limit_min/max` против **Host cap выбранного канала A/B** **при `enabled=true`** (ADR-021). `enabled=false` по умолчанию означает отсутствие нового ограничения для обоих UI (обычный `0..200` upstream), другие remote операции не блокируются по этому cap; реальная сила может превысить cap. См. [`11-remote-session-and-strength-cap.md`](11-remote-session-and-strength-cap.md).


`Bridge::Worker` един для local UI и remote clients; его текущая модель выбора — **один общий `active_device: usize`**, в отличие от независимого выбора устройства на терминале. `SwitchDevice(i)` меняет именно этот общий выбор. Удалённые команды `set_frequency`, `set_limit` и т.д. не принимают MAC и будут применены к *активному устройству на момент обработки Worker*. Не создавать иллюзию per-device targeting на API; любой альтернативный режим требует отдельного ADR. В v1 клиент отображает активный девайс по каждому новому snapshot и не кэширует индексы бесконечно.

## Неутверждённые детали для review

- Одна активная Remote Session (ADR-017), grace 30 s (ADR-022), heartbeat 5 s и detection ~15 s (ADR-023), ticket-based reconnect, fencing, Host kick и volatile secrets (ADR-024–027) **утверждены**. Открыты точные wire-контракты и криптографическое связывание transport/identity.
- Один master socket message type `command`, но детали `capabilities` и объём логов/телов остаются версиями в `schemas/`.
- Точный срок доступа токена, режим in-band vs Bearer WSS, TLS bind/config UX.

**Статус:** contract draft; не выдавать за подтверждённую реализацию или тестовую приёмку.


## Auth/revocation detail — behavior accepted v0.13

`13-session-auth-revocation.md` defines the **ACCEPTED** separation between password/new login, volatile resume ticket, session-scoped REST token, and one active transport generation (ADR-024–027). `auth.resume`, ticket rotation, Host kick and limited idempotent retransmission of the **same resume request** are accepted behaviors, not yet frozen wire fields. Before Host identity verification, no password, token or ticket may be transmitted. HTTP bearer/WS attachment shares the same operator slot. No token in WebView or URL. The JSON Schema/OpenAPI must now be updated and reviewed before coding.
