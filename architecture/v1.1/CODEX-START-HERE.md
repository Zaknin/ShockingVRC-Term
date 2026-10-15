# Codex handoff — ShockingVRC-Term v1.1 final architecture

**Do not treat delivery of this ZIP as a request to start coding immediately.** Await an explicit M1 implementation task from the owner. This file is a development handoff and authoritative scope map, not an execution script.

## Repository and source baseline

- Fork: `https://github.com/Zaknin/ShockingVRC-Term`
- Working branch: `feature/remote-access`
- Baseline commit: `e076bc2ede4726a976c60613031f48c0bc31e0b9` (ShockingVRC egui v1.3.1, Windows M0 137/137 tests PASS historically).
- The complete design package is this ZIP. Work from **one consistent revision**; never mix v0.17/v1.0.1 schemas with v1.1 documents.

## Read order

1. `ARCHITECTURE-APPROVAL-v1.1.md` and `APPROVAL-STATUS.md` — what the owner has actually approved.
2. `OWNER-AUDIT-RULES.md` — limits applicable when explicitly conducting a future audit. No new audits without instruction.
3. `docs/00-scope-baseline.md`, `docs/01-system-architecture.md`, `docs/06-command-matrix.md` — scope and complete command mapping.
4. `docs/14-remote-api-v1-normative-proposal.md`, `docs/15-data-model-and-operation-types.md`, `docs/19-api-approval-record.md`, `schemas/remote-messages-v1.schema.json`, `schemas/remote-models-v1.schema.json`, `schemas/direct-api-v1.openapi.yaml` — normative v1 Remote API.
5. `docs/20-host-remote-server-architecture.md`, `docs/21-host-backend-interfaces-and-dataflows.md`, `docs/23-host-server-review-record.md`, `docs/34-host-egui-remote-settings-ui.md` — Host.
6. `docs/24-tauri-terminal-architecture.md`–`docs/28-tauri-terminal-review-record.md`, `docs/35-tauri-ui-style-parity-and-flow.md` — Terminal.
7. `docs/29`–`33`, `schemas/rendezvous-v1.schema.json`, `deployment/` — BYO Rust rendezvous and coturn.
8. `docs/39-implementation-roadmap.md`, `docs/47-codex-implementation-task-briefs.md`, `docs/43-design-status-and-open-items.md` — milestones/engineering facts still unproven.

## Working rules

- No side-projects, major refactors, corporate SaaS requirements or speculative hardening.
- Do not change established BLE/OSC/Alarm/DSP/queue/power features as part of remote access.
- The Tauri Terminal is a separate remote-only client with no independent Engine; local egui remains functional.
- Do not auto-run functional/device/network/regression tests, or create deployments, without a separately approved scope from owner. A future milestone may explicitly authorize narrowly selected tests.
- No destructive commands; do not alter known working environments or OpenShock.
- Prefer smallest compatible change. Report hard blockers with exact code/contract evidence, explain the minimal workaround and wait for owner on an actual trade-off.
- Only 37 upstream commands are remotely allowlisted; updater remains local-only. `command.accepted` is proven enqueue, **not** applied value or physical success. Use new Snapshot for visible truth.
- One Remote Session (REST/WSS/RTC), 5 s heartbeat, ~15 s disconnect detection, 30 s reserved grace. Remote cap only applies to writes of channel A/B Power Limits min/max when enabled; disabled by default.
- Avoid shipping default personal infrastructure URLs, keys, passwords, IPs or TURN credentials.

## Acceptance and publication boundary

Code milestones, commits, SMB evidence publication, mirror upload, and hands-on verification are separately authorized actions. This documentation archive is not a code deliverable and not a claim of software functionality.
