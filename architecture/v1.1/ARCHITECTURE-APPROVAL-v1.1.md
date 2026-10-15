# ShockingVRC-Term — Final Architecture Approval v1.1

**Owner approval:** 2026-10-15. **Design status:** FINAL / APPROVED FOR PHASED IMPLEMENTATION. **Code status:** NOT IMPLEMENTED. **Baseline:** `Zaknin/ShockingVRC-Term` / `feature/remote-access` at `e076bc2ede4726a976c60613031f48c0bc31e0b9`.

## Scope of this approval

The owner expressly approved the full previously reviewed architecture as a whole, including:

- Remote API v1 and prior ADR-001–038 (ADR-033 **conditional feasibility target**, not proven).
- All nine Host proposals `HOST-P01`…`HOST-P09` (`docs/20`–`23`).
- All twelve Terminal proposals `TERM-P01`…`TERM-P12` (`docs/24`–`28`).
- Previously proposed self-hosted infrastructure/connection/UI/release/operation decisions `INF-P01`…`INF-P05`, `NET-P01`, `UX-P01`…`UX-P02`, `REL-P01`, `OPS-P01` in `docs/29`–`45` and `docs/43`.
- Windows-only Host and Terminal in initial release, Linux self-hosted Rust rendezvous and coturn, **blank endpoints and credentials in distributed defaults**, no shared hardcoded server.
- Tauri 2/Vue 3 interface stylistically close to current egui; same nine sections and 37 remote operations, without a second BLE/OSC engine.
- Owner-approved document corrections F-01…F-06 from v1.0.2, independently re-audited by an external reviewer/user-supplied report with **GO**, zero new blocking design findings.
- V2-01 optional interfile JSON Schema `$id`/`$ref` mismatch resolved by correcting the `remote-models-v1` declared `$id` to match existing relative references; **no DTO or wire contract change**.

## Hard implementation boundaries

1. Do not modify existing device-control algorithms (`src-core`, BLE, OSC, DSP, modulation, Alarm, current-power semantics or original Worker command queue) to implement remote networking. Use the GUI Bridge adapter as designed.
2. Keep a single active remote session across REST, WSS and RTC; preserve heartbeat 5 s, lost detection ~15 s, and 30 s resume grace; Host revoke semantics remain accepted.
3. Remote A/B strength caps are **optional per-channel constraints on remotely written Power Limits min/max only**, OFF by default; local GUI unrestricted, remote display shows actual Host values; caps are NOT physical-power bounds.
4. No hardcoded public STUN/TURN/rendezvous or private Moscow OpenShock service configuration. The reference Moscow deployment remains optional and is not carried in default builds.
5. ADR-033 requires checking that chosen TLS/RTC Rust libraries can support the approved host identity binding. If infeasible, present a concrete minimal alternative for separate owner approval; do not silently weaken identity or stall unrelated modules.
6. Do not launch implementation builds, functional tests, live networking, deployment or destructive commands merely because a design test plan exists. Owner separately controls timing and scope of hands-on verification. Static parsing and file-integrity checks of this documentation release are permitted and have been run.
7. The existing owner audit policy in `OWNER-AUDIT-RULES.md` remains authoritative for **audits**, distinct from eventual implementation tasks.

## Approval vs evidence

Approval records design intent. It does **not** prove that newly designed Host/Terminal/rendezvous code, TURN, TLS/DTLS, NAT traversal or any integration currently works. The historical M0 Windows baseline `cargo test --workspace` 137/137 PASS does not cover the unimplemented remote project.

## Canonical reading order

`OWNER-AUDIT-RULES.md` (audit instructions) → `CODEX-START-HERE.md` → `ARCHITECTURE-APPROVAL-v1.1.md` → `README.md` → `docs/00`, `05`, `06`, `14`–`19`, `20`–`28`, `29`–`47`, `schemas/`, `deployment/`.

## Historical status caveat

Older documents were authored when Host/Terminal design sections were `PROPOSED`. Their review-era wording is preserved as traceable history, but **this v1.1 approval record supersedes their approval state**, not their technical substance. ADR-033 remains conditional.
