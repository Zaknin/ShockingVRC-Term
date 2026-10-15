# 47. Ready-to-adapt Codex milestone briefs (not executed)

These briefs are **documentation for later coding**, not an instruction to start now. Each milestone explicitly preserves owner's non-destructive constraints. Auditor instruction will be supplied later. Before actual Codex work, confirm current branch/commit and apply any accepted audit findings.

## Common Codex header (applies to every milestone)

> Work only on `Zaknin/ShockingVRC-Term` fork's designated feature branch starting from validated `egui` baseline. Never change upstream `egui` directly; do not change BLE, OSC, Alarm, DSP, modulation, output rules, device power calculations or Worker 50ms loop. Do not run destructive device/router/VPS actions or unapproved migrations. Keep existing 137 M0 tests green and report newly added test totals. Do not silently alter accepted Remote API v1 wire schemas; describe any incompatibilities for owner decision. Limit deliverables and tests to the selected milestone. No actual deployment without explicit owner authorization.

### M1 — Host Remote Access

Read `20`–`23`, `34`, accepted API `14`–`19` and schemas. Create proposed `src-remote`, egui adapter/settings, one SessionManager, WSS+full companion REST. Snapshot/commands through one existing Bridge, no separate CliEngine. Enqueue result is not hardware apply. Verify Host `Remote Access OFF`, cap A/B and one-slot; gate identity verification per `33`. Deliver source diff, unit/contract test results and Windows localhost fixture proof. Do not attempt M4 ICE or Tauri UI within this task.

### M2 — Tauri Terminal Windows

Read `24`–`28`, `35`, `36`, `46`. Create remote-only Tauri 2 + Vue 3 program: Rust owns trust/auth/tokens/profile, Vue displays all nine egui sections and Connect. 37 operations and full Host snapshot, no direct DG-LAB/BLE/OSC. Wire identity before credentials; 5/15/30 liveness/recovery; current state authoritative. Deliver Windows build/testing report and installer candidate, not an automatic remote deploy.

### M3 — egui repaint optimization

Profile current baseline idle and minimized cost, then change **only GUI repaint scheduling** if measurements justify it. Target foreground 30Hz, unfocused 5Hz, minimized idle. Do not change Worker or engine 50ms timing. Before/after measurements needed; if no improvement, report and retain working original.

### M4a — Rust rendezvous/coturn reference deployment

Read `29`–`31`, `deployment/`, `schemas/rendezvous-v1.schema.json`. Create standalone Rust service, register/room/exchange and TURN ephemeral credential issuer; no ShockingVRC commands. Validate against fake clients and local coturn isolated fixture. Moscow server only after owner-authorized read-only inventory and explicit installation request.

### M4b — ICE/TURN adapters

Read `07`, `17`, `32`, `33`. Prove conditional ADR-033 with real chosen Rust WebRTC stack; verify DTLS fingerprint and Host identity proof. One shared JSON API/SessionManager across Direct and DataChannel. Test verified P2P and forced TURN; do not auto-login after kick, and do not retransmit unacknowledged commands blindly. No security proof claim absent C1–C5 test evidence.

### M5 — private release acceptance

Run `38`, build `36`, owner-approved L4 device tests separately. Produce source SHA, dependency versions, binary hashes, Windows resource measurements, feature parity matrix, known limitations, rollback description. Follow canonical SMB publishing workflow only if the owner requests it; no auto-upload or release from architectural audit context.

### M6 — optional local Tauri

Separate owner decision. Read `37`, measure performance and prototype local adapter without public networking dependency. Never replace egui just because Tauri Terminal exists.
