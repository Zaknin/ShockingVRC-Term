# 04. Этапы и критерии проверки — updated v0.5

**Status: Architecture/API v1 approved (ADR-001–038); no code changes yet (2026-10-15). ADR-033 requires feasibility/security review.**

## M0 — baseline (local PASS)

Fork `Zaknin/ShockingVRC-Term`, branch `feature/remote-access` @ `e076bc2ede4726a976c60613031f48c0bc31e0b9`. Windows `cargo test --workspace` → 137/137 PASS. Не было проверки физического устройства в этом тестовом прогоне. Evidence на SMB ещё не опубликован.

## M1 — core Remote API direct HTTPS/WSS
**Add to M1 scope (decided 2026-10-15):** Host-level one remote session, live sync with local egui, Host-only Option A permission check for remote `Power Limits min/max` writes only (accepted); no checks added to other remote operations. These are API authorization and read-model features, not changes to the physical output engine.


`src-remote` + GUI adapter, auth, API DTO, versioned JSON envelopes, identity pinning, Remote Access host settings, direct listener, optional UPnP/NAT-PMP/PCP. Unit + integration tests на detached Bridge. **Никаких** изменений в BLE, OSC, Engine, Alarm, modulation, limits. Гарантировать аутентификацию *до* выдачи private state. `command.accepted` только для реально подтверждённого enqueue, иначе `received`.

## M2 — Tauri Remote Terminal

Tauri Rust transport + Vue, Host profile and credential storage, direct HTTPS/WSS, login/logout, snapshot and capability, все команды `06-command-matrix.md`, список пресетов и logs, reconnect. Visual parity оценивается по доступным настройкам, а не pixel-perfect egui.

## M3 — egui FPS limiter

Цель focused 30 FPS / unfocused 5 / minimized 0 render FPS. GUI-only request/render gating, Worker 50ms не изменять. Измерить CPU/GPU до/после на одном и том же хосте.

## M4 — ICE/STUN/TURN traversal + signaling

Self-hostable rendezvous/signaling reference deployment + coturn integration, пользовательские endpoint profiles **без preconfigured URLs**, host+terminal ICE, WebRTC reliable ordered DataChannel, TURN fallback и identity binding. **Те же** command/snapshot frames, никакого дублирования Worker. Check NAT cases: direct ICE, forced TURN, signaling failure, identity mismatch, reconnect.

## M5 — полные LAN/VPN/WAN/CGNAT испытания и release candidate

Direct WAN через UPnP либо manual port forward с внешним клиентом. Проверка DDNS change, TLS pin, mixed networks, TURN relay, credentials, restore window, auto reconnect, full read/write command coverage. Все исходные 137 unit/render тестов должны остаться PASS; новые — сверх них. Результаты сбоя публикуются наряду с успешными.

## M6 — последующая замена Host egui на Tauri

Опциональный отдельный milestone **после** стабильного Remote Terminal, без изменения алгоритмов устройства. Использовать тот же Vue frontend, локальная оболочка может быть отдельным режимом. Не начинать в M1–M5.

## Архитектурные gates до реализации

1. **DONE — ACCEPTED (design):** transport-neutral API v1 contract + command matrix (ADR-028–032).
2. **DONE — ACCEPTED:** one remote operator; shared `active_device` semantics retained and documented (ADR-017/038).
3. BYO infrastructure **утверждена**. Разработать pairing/Host identity binding, deployment docs и проверить отдельный тестовый московский deployment на сервере с OpenShock, не связывая приложения.
4. **DONE — ACCEPTED project scope:** ICE/TURN included as M4; detailed signaling deployment + RTC security must be validated before M4 completion.
5. **PORT ACCEPTED:** 4318 TCP (ADR-036); exact Host Remote Access UI fields, bind defaults and implementation remain for M1 design.

## Evidence publication

Canonical root: `\\\<evidence-share>\Torrents\Codex\ShockingVRC-Term\<M#>\<timestamp>\`.
Любой успешно полученный diagnostic/test artifact публикуется независимо от PASS/FAIL/BLOCKED. `.part` → read-back → SHA-256 source vs read-back → final rename/commit → `.sha256` sidecar last. Google Drive `/Codex/ShockingVRC-Term` — downstream mirror, не место canonical commit. **Эти документы пока не опубликованы в SMB и не закоммичены в GitHub.**
