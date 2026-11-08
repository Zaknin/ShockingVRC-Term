# P5 Increment 2: Editable Remote Graph Workspace Implementation Plan

> Execute one bounded slice at a time. Write its failing tests first, implement the smallest viable change, run its focused tests, then commit that slice.

**Goal:** Add an editable Terminal Graph workspace backed by the Host's Rust Graph schema and evaluator, with local drafts and explicit revision-checked apply.

**Architecture:** Reuse `shocking_vrc_core` model/catalog/preset/compile functionality from Terminal Tauri without running a Terminal evaluator. Keep draft state local and persistent. Transfer selected Host presets and Graph drafts in bounded authenticated chunks. Apply through a serialized Host command after Host validation and an execution-time revision check; Terminal waits for a newer authoritative Graph document before reporting confirmation.

**Spec:** `docs/superpowers/specs/2026-11-08-p5-remote-graph-parity-increment-2-design.md` and the user-provided P5 brief.

## Constraints

- Host remains the only Graph evaluator and device-output authority.
- Dragging, editing, preset selection, save, and import never change Host state.
- Apply requires an explicit confirmation and base revision; queue acceptance alone is not success.
- Keep the existing 256 KiB API frame and 4 MiB Graph document bounds.
- Reject stale revisions, invalid or unsafe Graph payloads, oversized uploads, malformed chunks, foreign or expired transfer IDs, and repeated commit requests safely.
- Preserve panic latch behavior and add no panic release operation.
- Use synthetic test backends only; never activate physical outputs.

## Tasks

### Task 1 — Shared Graph schema and Terminal draft services

Files: `src-terminal/src-tauri/Cargo.toml`, `src-terminal/src-tauri/Cargo.lock`, `src-terminal/src-tauri/src/**`, `src-terminal/src/domain/**`.

- [x] Reuse `shocking_vrc_core::graph::compile::compile` to obtain existing Host `GraphIssue` values without evaluating outputs.
- [x] Add Tauri schema/canvas descriptors from `kind::catalog()`, `NodeKind::inputs/outputs`, and the existing serializers.
- [x] Add bounded parse/normalize/validate commands and per-Host-profile local draft save/load with atomic replacement.
- [x] Add unit tests for schema parity, validation, size/depth limits, isolation by profile, and persistence recovery.
- [ ] Commit the Rust draft service.

### Task 2 — Authenticated preset detail transfer and bounded Graph apply protocol

Files: `src-remote/src/protocol.rs`, `src-remote/src/operations.rs`, `src-remote/src/api.rs`, `src-gui/src/remote_adapter.rs`, `src-gui/src/bridge.rs`, `src-app/src/graph_runtime.rs` only if a worker seam is needed, and focused integration tests.

- Add a session-bound bounded transfer for a selected Graph preset.
- Add authenticated Graph upload start/chunk/commit/status frames. Keep transfer ids bound to server session, auth generation, and session; cap bytes, chunk count, transfer age, pending commits, and frame size.
- Validate JSON depth, Graph version, node kinds, ids, references, pin directions/keys, cycles, nested groups, finite values, and compile errors on Host.
- Add a serialized Bridge worker command that compares the submitted base revision immediately before replacement, then uses the existing `GraphRuntime::set_graph` path and returns applied/rejected/conflict status.
- Prevent duplicate commits, preserve request replay rules, and ensure status queries cannot enqueue writes.
- Test stale revision, mode change races, malformed/oversized data, bad offsets/checksums, session crossing, panic preservation, and output-free synthetic fixtures over WSS and DataChannel.
- Commit the protocol/Host slice.

### Task 3 — Terminal Graph draft lifecycle and transport commands

Files: `src-terminal/src-tauri/src/commands.rs`, `src-terminal/src-tauri/src/protocol.rs`, `src-terminal/src-tauri/src/transport/direct_wss.rs`, `src-terminal/src/domain/**`, `src-terminal/src/stores/terminal.ts`.

- Add selected-preset fetch, Graph upload, and apply-status poll commands over the active SessionSocket.
- Confirm apply only after a newer Host document revision and canonical graph hash match.
- Preserve local drafts across disconnect/reconnect; mark them stale after newer Host revisions or a new Host session. Never auto-merge or retry an unknown apply.
- Test both transports, request ids, checksum/length bounds, fresh confirmation, stale conflict, timeout unknown, no replay, and draft retention.
- Commit the Terminal transport/state slice.

### Task 4 — Interactive Vue Graph editor and local file/preset flows

Files: `src-terminal/src/components/GraphWorkspace.vue`, new Graph canvas/inspector components, `src-terminal/src/App.vue`, `src-terminal/src/stores/terminal.ts`, `src-terminal/src/style.css`, and Node/DOM tests.

- Replace the read-only list with an SVG canvas using Host-derived node/pin metadata.
- Implement node add/remove/move/select, typed wiring, canvas pan/zoom/frame, defaults and parameter inspector, preset replace/insert, grouping, duplication, and bounded undo/redo.
- Add local save/reload, validated import/export, dirty/stale/invalid indicators, and an explicit apply confirmation showing the Host revision and runtime mode.
- Preserve the existing mode selector and read-only panic display; provide no panic release.
- Add deterministic UI tests for all controls and failure states.
- Commit the UI slice.

### Task 5 — Full Increment 2 verification and evidence

- Run formatting, focused tests, full Rust workspace tests, Terminal Tauri tests, Node tests, and production build.
- Run source/dependency security checks and inspect the entire branch diff.
- Produce and publish a sanitized report with exact revisions, commands, transport results, manual GUI/WAN boundaries, and hashes.
- Push only the dedicated P5 branch after local source-security and applicable CI equivalents pass; do not open a PR, change `main`, deploy, or install.
