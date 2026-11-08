# P5 Increment 1: Remote Mode and Graph Observation Implementation Plan

> For agentic workers: REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox syntax for tracking.

**Goal:** Add authenticated mode switching and a read-only remote Graph workspace over the existing WSS and WebRTC DataChannel protocols.

**Architecture:** Extend the current authenticated frame protocol additively and advertise Graph features only when the backend supports them. Reuse the Host Bridge queue and GraphRuntime; transfer graph JSON in bounded, session-bound chunks. Terminal treats command acceptance as enqueue only and waits for authoritative Host state.

**Tech Stack:** Rust workspace, serde/serde_json, Tokio/Axum, Tauri 2, Vue 3, TypeScript, Node test runner.

**Spec:** docs/superpowers/specs/2026-11-08-remote-graph-parity-p5-design.md and the user-provided P5 brief.

## Global Constraints

- Source baseline is 9b1e0ae02804a2a80833b70afb8f38d813480fcb on the dedicated P5 branch.
- Do not change the existing 256 KiB API frame limit.
- Preserve authentication, capability negotiation, rate limits, replay protection, session isolation, and argument validation.
- Reuse Host's serialized mode and command mechanisms; do not add a Terminal graph evaluator.
- Terminal mode completion comes from a fresh Host snapshot, not queue acceptance.
- Older Hosts without Graph capabilities remain usable and show Graph controls as unavailable.
- Host remains the only graph execution and device-output authority. Remote clients cannot release Host Graph Panic.
- Use synthetic fixtures only; no hardware output tests.

## Review Focus

- Older Host has no Graph features: retain mode visibility and keep unsupported actions disabled; pin this to the capability fallback test in Task 4.
- Queue acknowledgement arrives before mode state: show pending until a newer snapshot confirms the target; timeout/reconnect remains unknown and must not replay; pin this to Task 5 store tests.
- Classic writes race a queued Graph transition: preserve Bridge queue ordering and reject writes queued after the mode transition; pin this to Task 2 adapter/Bridge tests.
- Graph payload is too large, malformed, or a transfer offset is invalid: reject within existing limits and bind every chunk read to the authenticated session; pin this to Task 3 API tests.
- Graph is panicked or Graph capability is absent: display state only, with no remote panic-release surface; pin this to Task 6 view tests.

---

### Task 1: Define the additive Graph protocol and capability contract

**Files:**
- Modify: src-remote/src/operations.rs
- Modify: src-remote/src/protocol.rs
- Modify: src-remote/src/lib.rs
- Test: src-remote/src/protocol.rs
- Test: src-remote/src/operations.rs

**Interfaces:**
- Add operation engine.set_mode with exactly one argument, mode, accepting classic or graph.
- Add Graph document metadata, validation issue, and preset-summary DTOs.
- Add default unsupported RemoteBackend methods for graph document reads and Graph preset summaries so existing test backends remain compatible.
- Keep the existing 37 operations stable and expose engine.set_mode only as a negotiated Graph extension.

- [x] Write tests for mode argument validation, unknown/extra fields, feature-aware operation lists, and legacy backend defaults.
- [x] Run the focused protocol and operation tests and confirm the new cases fail for the expected missing behavior.
- [x] Implement the operation and DTO contract with strict input validation.
- [x] Run the focused Rust tests and confirm the new contract passes without changing MAX_FRAME_BYTES.
- [x] Commit the protocol contract.

### Task 2: Adapt Host Bridge state and mode command

**Files:**
- Modify: src-gui/src/remote_adapter.rs
- Modify: src-gui/src/bridge.rs only if a new test seam is required
- Test: src-gui/src/remote_adapter.rs
- Test: src-gui/src/bridge.rs

**Interfaces:**
- BridgeRemoteBackend advertises Graph read and mode features.
- Its graph document method reads the current GraphDocShared revision, GraphRuntime status and graph document; it does not execute graph evaluation.
- engine.set_mode maps to the existing Command::SetMode through Bridge::try_send.
- Classic configuration operations continue to use try_send_classic_configuration.

- [x] Write Host adapter tests for authoritative graph revision/status, preset summaries, and engine.set_mode mapping.
- [x] Add a queue-order test proving a Classic write is rejected after SetMode(Graph) has entered the shared queue.
- [x] Run those focused tests and confirm they fail for the missing Graph adapter behavior.
- [x] Implement only the adapter conversion and mapping; retain the existing Worker and GraphRuntime execution path.
- [x] Run the Host adapter and Bridge tests.
- [x] Commit the Host adapter slice.

### Task 3: Add authenticated, bounded graph reads to the shared API

**Files:**
- Modify: src-remote/src/api.rs
- Modify: src-remote/src/protocol.rs
- Test: src-remote/src/api.rs

**Interfaces:**
- Add authenticated graph.document.get, graph.document.chunk, and graph.presets.get request frames and matching request-id replies.
- Begin transfer returns a session-bound transfer ID, graph revision, total byte length, fixed chunk length, SHA-256, and editor metadata.
- Chunk replies carry base64 data and exact offsets; use a conservative chunk size whose encoded frame stays below MAX_FRAME_BYTES.
- Bound total document bytes, active transfers per session, and transfer lifetime. Reject expired IDs, foreign-session IDs, invalid offsets, bad max_bytes, and checksum mismatch.
- Capability response advertises graph.document.read.v1, graph.presets.catalog.v1, engine.mode.set.v1, and engine.set_mode only when the backend supports them.

- [x] Write authenticated API tests for document start/chunks, preset catalog, and unsupported-backend capability negotiation.
- [x] Write negative tests for unauthenticated access, oversized documents, expired/foreign transfer IDs, invalid offsets, malformed arguments, and response frame bounds.
- [x] Run focused API tests and confirm the new behavior fails before implementation.
- [x] Implement frame parsing, auth-generation binding, bounded transfer storage, cleanup, and response-size checks without changing the global limit.
- [x] Run focused API tests and existing WebSocket/DataChannel request tests.
- [x] Commit the API slice.

### Task 4: Carry Graph requests and authoritative mode through Terminal Rust

**Files:**
- Modify: src-terminal/src-tauri/src/protocol.rs
- Modify: src-terminal/src-tauri/src/commands.rs
- Modify: src-terminal/src-tauri/src/transport/direct_wss.rs
- Modify: src-terminal/src/protocol/types.ts
- Test: src-terminal/src-tauri/src/transport/direct_wss.rs
- Test: src-terminal/src-tauri/src/commands.rs

**Interfaces:**
- Add engine_mode to the Terminal snapshot DTO so Vue receives the Host's authoritative field.
- Add a Graph request action routed over the currently active WSS or RTC SessionSocket, with request IDs and bounded replies.
- Add Tauri commands that fetch and verify the full chunked document and preset summaries, and submit engine.set_mode.
- Preserve the current unknown-outcome message and no-replay behavior on acknowledgement timeout.
- Old Hosts may omit Graph features; parse the capability list without failing the session.

- [x] Write tests for engine_mode deserialization, Graph request/reply correlation, chunk checksum/length, and legacy capabilities.
- [x] Write a no-replay test for a mode acknowledgement timeout.
- [x] Run focused Terminal Rust tests and confirm missing behavior.
- [x] Implement the new request flow for both SessionSocket variants and bind it to the existing session.
- [x] Run focused tests plus current reconnect and transport protocol tests.
- [x] Commit the Terminal transport slice.

### Task 5: Add pending mode control and a read-only Graph workspace

**Files:**
- Modify: src-terminal/src/App.vue
- Modify: src-terminal/src/stores/terminal.ts
- Modify: src-terminal/src/domain/ui-preferences.mjs only if navigation constants require it
- Create: src-terminal/src/components/GraphWorkspace.vue
- Modify: src-terminal/src/style.css
- Test: src-terminal/src/domain/graph-session.mjs
- Test: src-terminal/src/domain/graph-session.test.mjs

**Interfaces:**
- Add a Graph section that shows nodes, connections, graph revision, validation issues, and panic state from Host.
- Add a Classic/Graph mode selector when engine.mode.set.v1 is advertised.
- Mode action status is queued/pending until a fresh snapshot confirms the requested mode; rejected, timed out, stale, reconnecting, and unsupported states are visible and never blindly retried.
- Graph workspace is read-only in this increment. Panic status has no release control.

- [x] Add domain tests for mode acknowledgement versus confirmed snapshot, competing Host state, reconnect, timeout, and unsupported capability behavior.
- [x] Run the focused Node tests and confirm they fail for the new state model.
- [x] Implement store state and Vue view using the new Tauri commands.
- [x] Add component tests or deterministic render tests for node/connection visibility, validation messages, unsupported Host, and panic read-only behavior.
- [x] Run focused Node tests and frontend type/build checks.
- [x] Commit the read-only Terminal slice.

### Task 6: Verify Increment 1 as a cross-transport vertical slice

**Files:**
- Modify: focused existing API/Terminal integration tests as needed
- Create: sanitized evidence report under docs or deliverables

- [x] Run the same authenticated Graph read and mode request through the production WebSocket fixture.
- [x] Run the same frames through the production WebRTC DataChannel fixture.
- [x] Verify old-Host capability fallback, mode confirmation from fresh state, Classic-write queue guard, panic read-only state, and bounded transfer rejection.
- [x] Run relevant Rust workspace and Terminal test/build suites.
- [x] Review git diff and ensure only P5 implementation files and supporting design, plan, and report are present.
- [x] Record exact commands, revisions, test outcomes, and unverified GUI/WAN acceptance in the report.
