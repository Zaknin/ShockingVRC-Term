# 37. Optional later migration of local egui to Tauri

**Status:** FUTURE OPTION, NOT v1 REQUIREMENT. AmigoFPS has discussed translating the local interface to Tauri; we are delivering a remote Tauri Terminal first. This document avoids architectural dead ends without committing to replacing egui.

## Preconditions

Remote-only Terminal must be functional and pass equivalence tests. Only then decide whether a local Tauri shell would be worth the cost. Tauri is not inherently faster than egui: local WebView2 + Vue resource use must be measured side-by-side with throttled egui.

## Reuse boundaries

Reusable: Vue views/controls, styling tokens, DTO validators, Rust protocol formatting/helpers, UI state store with abstract `HostAdapter`. Do **not** make Vue components call network-only WSS directly; provide a typed adapter interface for `RemoteHostAdapter` vs hypothetical `LocalHostAdapter`.

Local Tauri shell (future) would call an in-process Rust backend adapter in the Host process (Tauri IPC), not loop packets through a public port or start a second engine. The original single AppState/Worker owner remains the authority. Migration does not grant another remote session and does not change underlying BLE/OSC/Alarm.

## Possible later phases

1. Visual feasibility prototype of local Tauri shell rendering Status using fixture state; no hardware.
2. Measure CPU/GPU/RAM, foreground 30 FPS target, unfocused 5 FPS target, minimized idle; compare to current egui on same Windows host.
3. Implement local-only Rust adapter for common Vue state/commands and restore each screen's functional parity.
4. Parallel testing of egui and Tauri on **separate controlled builds**, not concurrently managing the same physical Coyote.
5. Owner approves whether to replace egui, retain both interfaces, or drop local migration entirely.

## Not allowed as an accidental side effect of Remote v1

Deleting egui, changing original Worker to event-driven for GUI convenience, rewriting BLE/OSC/Alarm/DSP, starting a second Engine, changing current safety/power calculations, or treating remote-server protocol as an unconditional dependency for local operation.
