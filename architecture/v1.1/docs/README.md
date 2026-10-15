> **Current v1.1 notice:** Host/Term/Infrastructure detailed architectures were approved together by the owner after this historical v0.17 index was written. This document remains a historical index. See root `../APPROVAL-STATUS.md` and `../ARCHITECTURE-APPROVAL-v1.1.md`. ADR-033 remains conditional.

# ShockingVRC-Term — Architecture v0.17 (Remote API v1 APPROVED; Host + Tauri architectures PROPOSED)

**ADR-001–038 приняты как проектные решения 2026-10-15 (ADR-033 — условно: криптографическая реализуемость и proof должны пройти проверку). Код и инфраструктура не реализованы.**

**Начать обзор Terminal с:** [`24-tauri-terminal-architecture.md`](24-tauri-terminal-architecture.md), [`25-tauri-terminal-ui-and-state.md`](25-tauri-terminal-ui-and-state.md), [`26-tauri-transport-ipc-and-storage.md`](26-tauri-transport-ipc-and-storage.md), [`27-tauri-terminal-validation-and-delivery.md`](27-tauri-terminal-validation-and-delivery.md) и [`28-tauri-terminal-review-record.md`](28-tauri-terminal-review-record.md). Host-архитектура на рассмотрении: [`20-host-remote-server-architecture.md`](20-host-remote-server-architecture.md)–[`23-host-server-review-record.md`](23-host-server-review-record.md). Предыдущий утверждённый API: [`19-api-approval-record.md`](19-api-approval-record.md), [`14-remote-api-v1-normative-proposal.md`](14-remote-api-v1-normative-proposal.md).

Новые в v0.14 документы: `14` wire/API, `15` typed DTO, `16` test gates, `17` identity/RTC, `18` decision slate; в v0.15 — `19` approval record. Новые schema файлы: `remote-models-v1.schema.json`, `remote-messages-v1.schema.json`; `examples/*.json`. Старые схемы v0.13 сохранены как historical drafts.


**Новые в v0.16:** Host Server architecture — **PROPOSED** (`HOST-P01`–`HOST-P09`), одобрение v0.15 по-прежнему в силе.

**Новые в v0.17:** `24`–`28` — Tauri Remote Terminal application architecture, Vue UI/functional parity, transport/IPC/secret boundaries, Windows test plan, review record `TERM-P01`–`TERM-P12` **PROPOSED**. Аудит будет подготовлен только после завершения полного пакета документов и инструкции владельца. Ничего не реализовано.

**Дата:** 2026-10-15  
**Форк:** https://github.com/Zaknin/ShockingVRC-Term  
**Upstream:** https://github.com/nullshii/ShockingVRC/tree/egui  
**Baseline:** `e076bc2ede4726a976c60613031f48c0bc31e0b9`  
**Development branch:** `feature/remote-access`  
**Статус:** API v1 — **ACCEPTED**; архитектуры Host и Terminal — **PROPOSED FOR REVIEW**; это **не** реализация. Security feasibility, Host architecture, deployment и код требуют последующей работы.

| Файл | Назначение |
|---|---|
| `00-scope-baseline.md` | Границы, инварианты, baseline |
| `01-system-architecture.md` | Host / Terminal / Bridge и lifecycle |
| `02-api-auth-protocol.md` | Детальный Remote API v1: handshake, auth, DTO, REST/WS |
| `03-connectivity-tls-upnp.md` | LAN/VPN/WAN, DDNS, TLS pin, UPnP, ICE |
| `04-milestones-validation.md` | M0–M6, acceptance, evidence workflow |
| `05-decision-log.md` | Принятое и открытые ADR |
| `06-command-matrix.md` | **Все upstream GUI-команды** → Remote API ops |
| `07-ice-turn-signaling.md` | ICE/STUN/TURN, WebRTC data channel и public signaling |
| `08-api-contract-tests.md` | Контрактные, сетевые, negative и regression tests |
| `09-session-flow.md` | State machine, вход, потеря сети, ACK semantics |
| `10-self-hosted-connectivity.md` | BYO signaling/STUN/TURN, пустые defaults, частная московская инсталляция |
| `11-remote-session-and-strength-cap.md` | Одна remote session, live sync и **Option A: cap только для удалённой записи Power Limits min/max** |
| `12-session-lifecycle-recovery.md` | **30 с grace, 5 с heartbeat и ~15 с detection ACCEPTED**; resume ticket, fencing и Host revoke также ACCEPTED ADR-024–027 |
| `13-session-auth-revocation.md` | **ACCEPTED BEHAVIOR:** credential/token/ticket separation, atomic resume fencing, Host kick/revoke, REST+WSS one-slot binding; wire format закреплён в `14-*` и `15-*`; сам исходный документ исторический |
| `14-remote-api-v1-normative-proposal.md` | Утверждённый v1 wire contract (план, не реализация) |
| `15-data-model-and-operation-types.md` | Утверждённые DTO и enum mapping |
| `16-contract-tests-and-examples.md` | Утверждённый план проверок (ещё не выполнялись) |
| `17-host-identity-and-transports.md` | Криптографическая конструкция (условно принята, необходим security review) |
| `18-proposal-decisions-v1.md` | ADR-028–038: утверждённые решения |
| `19-api-approval-record.md` | Реестр утверждения Remote API 2026-10-15 |
| `20-host-remote-server-architecture.md` | **NEW v0.16:** модули Host, ownership, startup/shutdown, network, command gateway |
| `21-host-backend-interfaces-and-dataflows.md` | **NEW v0.16:** backend adapter, Rust границы, DTO, сессии, concurrency |
| `22-host-validation-and-operational-plan.md` | **NEW v0.16:** тесты, Windows verification, acceptance gates |
| `23-host-server-review-record.md` | **NEW v0.16:** предложения HOST-P01–P09 для утверждения |
| `24-tauri-terminal-architecture.md` | **NEW v0.17:** Tauri/Rust/Vue modules, process/session ownership, direct + RTC transport boundaries |
| `25-tauri-terminal-ui-and-state.md` | **NEW v0.17:** nine views, 37-operation functional parity, state/UI reconciliation |
| `26-tauri-transport-ipc-and-storage.md` | **NEW v0.17:** typed IPC, WSS/RTC, trust, credential placement and errors |
| `27-tauri-terminal-validation-and-delivery.md` | **NEW v0.17:** Windows integration scenarios, fixtures, tests, packaging and release measurements |
| `28-tauri-terminal-review-record.md` | **NEW v0.17:** TERM-P01–P12 proposals for owner review; no audit methodology imposed |
| `../schemas/remote-envelope.schema.json` | Машиночитаемая проектная схема WS/RTC message envelope |
| `../schemas/direct-api.openapi.yaml` | Машиночитаемый эскиз direct HTTP API |
| `../schemas/connectivity-defaults.example.json` | Нулевые defaults настроек пользовательской инфраструктуры |

**Принято в v0.4:** ICE/TURN входят в scope. **BYO infrastructure**: в поставке нет преднастроенного rendezvous/STUN/TURN и чужих TURN credentials. Пользователь настраивает свои конечные точки; пример частной установки — московский сервер, где уже запущен OpenShock. Использование его в продукте по умолчанию запрещено. Transport/data-channel и identity proof пока требуют согласования. Прямой WSS полностью независим от этих сервисов.
**В v0.5 согласованы:** одна Remote Session, двусторонняя синхронизация и Host-defined Remote Maximum Strength. **В v0.6 принят Option A:** удалённый потолок применяется **только к записи `Power Limits` (`limits.min/max`)**, не к зонам, модуляции, пресетам или Alarm. Фактическая сила может быть выше этого значения; оно **не является аппаратным лимитом**. **Новое в v0.7:** `enabled=false` по умолчанию принят. При выключенной опции обе стороны свободно выставляют Power Limits в диапазоне `0..200` по исходным правилам; числовой cap не применяется. **Новое в v0.8:** утверждены два независимых числовых remote caps A и B (`ADR-021`) под общим `enabled` переключателем; только соответствующий канал проверяется Host API.

**В v0.9–v0.11:** согласованы одна remote session, 30-секундный grace **после обнаружения обрыва** (ADR-022), heartbeat 5 s, detection ~15 s (ADR-023). **Новое в v0.13:** отдельно приняты ticket-based resume/fencing, Host revoke без скрытого auto-login, secret placement и bounded idempotent duplicate resume response (ADR-024–027). См. `12-session-lifecycle-recovery.md` и `13-session-auth-revocation.md`.

**Важно:** корневой протокол `Command`/`Snapshot` остается связанным с одним существующим `src-gui::bridge::Worker`; ни BLE, ни OSC, ни Alarm, ни очередь исполнения в `src-core` не меняются. `Bridge::send()` исходно не подтверждает применение — ACK отделён от Snapshot.

**M0:** Windows local `cargo test --workspace` 137/137 PASS. Этот архив — подготовленный локальный draft, не evidence PASS, не канонический SMB publication, не GitHub commit.

**Публикация после implementation/evidence preparation:** сначала `\\\<evidence-share>\Torrents\Codex\ShockingVRC-Term\...`, независимый read-back и SHA-256, sidecar last; Google Drive `/Codex/ShockingVRC-Term` — downstream mirror.

**В v0.12:** описан подробный проект session auth/revoke. **В v0.13:** пользователь утвердил поведение (ADR-024–027), включая идемпотентный ответ на повтор того же resume после потери ACK. **В v0.15:** пользователь утвердил API v1 wire contract, REST companion, DTO, 37 ops, TTL/cache/rate limits и тестовые требования (ADR-028–038). JSON Schema/OpenAPI остаются проектными входными материалами для реализации, не generated/executed API. Host identity binding принят условно до prototyping/review.
