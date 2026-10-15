# 39. Execution roadmap — Codex implementation after documentation review

**Status:** Proposed sequence, NO implementation authorized by this document. The owner will commission an independent architectural audit later with bespoke instructions. This roadmap is for post-review execution only.

## M0 — baseline (already done)

- Repository: `Zaknin/ShockingVRC-Term` branch `feature/remote-access` starting at `e076bc2ede4726a976c60613031f48c0bc31e0b9`.
- Windows `cargo test --workspace`: **137/137 PASS**, historical test baseline. Host executable works at Nightmare according to owner. Physical device retest was not included in the automated test result.
- Keep upstream `egui` unmodified; avoid upstream `main` as source of feature work.

## M1 — Host direct Remote API

1. Implement `src-remote` crate + egui bridge adapter and Host local UI.
2. Auth/session manager with full approved REST/WSS API and typed DTO.
3. Host-only independent A/B cap behavior and server port lifecycle.
4. Pairing/TLS identity proof gate C1 for Direct, if not feasible stop for decision rather than bypass.
5. Tests L0/L1/L2; local Host features unchanged. **Exit:** direct `localhost/LAN` fixture Term can connect, all relevant 37 commands reachable via API, legacy tests pass.

## M2 — Windows Tauri remote-only Terminal

1. Rust SessionClient + Direct WSS identity validation + credential/profile store.
2. Vue 3 egui-inspired 9 screens + Connect view; typed protocol adapters.
3. Real Host snapshot and 37 command UI completeness; 5/15/30 recovery.
4. Windows installer and portable build as proposed, sanity tests.
5. **Exit:** Windows Host↔Terminal functioning on LAN/VPN without exposing a new engine.

## M3 — Host egui repaint FPS

1. Profile current repaint and OS resource use before changes; identify present idle/minimized behavior empirically.
2. If justified, target foreground 30 FPS, unfocused 5 FPS, minimized idle, without altering 50ms Worker processing (source egui event capability must be proven).
3. **Exit:** measured foreground/background/minimized CPU/GPU and unchanged engine timing; preserve baseline. This is UI-only, not an engine refactor.

## M4a — self-hosted rendezvous + coturn

1. Implement `src-rendezvous` Rust signaling service and schema tests.
2. Prepare Linux deployment templates, operator-generated secrets and TURN REST ephemeral credential flow.
3. Run isolated setup (test VPS or approved Moscow server after read-only inventory).
4. **Exit:** Host/Term signaling exchange and TURN credentials validated without ShockingVRC hardware.

## M4b — ICE/WebRTC DataChannel adapter

1. Prove ADR-033 C2–C5 with chosen Rust RTC library and peer identity binding.
2. Add RTC adapters on Host and Terminal, same approved JSON frames.
3. Auto mode chooses Direct first, ICE P2P then TURN; one session across paths.
4. **Exit:** LAN ICE, NAT P2P where feasible, forced TURN, CGNAT-like test, identity mismatch rejection and graceful fallback.

## M5 — polish, packaging, private friend testing

- Complete 37-op parity, direct/RTC errors, Windows DPI, reconnect edge cases, resource measurement, upstream regression and private pairing tests.
- Release tagged fork binaries with source/license references, evidence and hashes; do not claim public operational readiness.

## M6 — optional local Tauri migration

Separate decision **after** successful M5. Reuse Vue UI with local adapter; not required in first remote release. No unconditional local egui replacement.

## Change boundaries

- Allowed: additive `src-remote`, `src-terminal`, `src-rendezvous`, new docs/config, thin `src-gui` bridge adapter/settings, optional scoped egui repaint throttling.
- Disallowed absent new explicit approval: changes to `src-core`, BLE/OSC/Alarm/DSP/modulation/engine/watchdog semantics, replacing queue, forced safety/power changes, background resident client, global cloud service, default public URLs.
- One milestone per Codex task; each must state changed paths, tests, known limitations and reversible scope. No unattended package deployment or router modification in this documentation stage.

## Handoff after owner-defined audit

Once the owner supplies audit instructions, that process may produce findings. Incorporate only findings the owner accepts. Resolve code/spec mismatches by explicit ADR change, not by silently editing previously approved v0.15 API wire schemas.
