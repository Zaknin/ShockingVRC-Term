# ShockingVRC P5 — Remote Graph Parity Design

## Intent

Bring the Windows Remote Terminal toward the Host Graph Editor by adding a real remote Graph workspace. Host remains authoritative for engine mode, graph documents, validation, presets, persistence, evaluation, and all device output.

## Goals

- Implement the user-specified work in four vertical increments: authenticated mode and graph observation; an editable local draft; revision-safe explicit Apply; and broad integration/UI verification.
- Make Classic/Graph mode visible and switchable from Terminal, with completion determined by a later Host snapshot.
- Show the Host graph document, revision, validation issues, editor metadata, and preset catalog.
- Support Graph document transfer without raising the existing 256 KiB frame limit.
- Reuse the existing Graph schema, node definitions, validator/runtime, auth/session, command queue, WSS, and WebRTC DataChannel.
- Preserve drafts and reconcile them with current Host state after disconnects, reconnection, device discovery, or concurrent Host edits.

## Non-goals

- No second graph evaluator or device-output path in Terminal.
- No unauthenticated Graph listener, LAN-only endpoint, new auth/session pool, or bypass of existing transport protections.
- No remote release of a latched Host Graph panic.
- No automatic application of draft edits or parameter streaming while Graph Mode is active.
- No changes to Remote Power Limits policy, BLE/OSC behavior, hardware output tests, or the global frame size.

## Approaches

1. Add a separate Graph HTTP service. Rejected because it splits the established auth/session path and cannot serve the RTC DataChannel transport.
2. Extend the existing authenticated Remote API frames and command path. Selected because both WSS and RTC already carry the same bounded JSON protocol, and Host Bridge already serializes mode and configuration commands.
3. Reimplement Graph evaluation in Terminal. Rejected because Host GraphRuntime is the single execution authority and already owns validation and device handling.

## Architecture

- Add an additive, capability-negotiated Graph extension to Remote API v1. Older Hosts continue to provide the existing 37 operations and are shown as lacking Graph workspace support.
- Use authenticated request/reply frames for graph metadata, bounded document transfer, and preset catalog metadata. Keep every frame at or below MAX_FRAME_BYTES; bound total document size, chunk size, transfer lifetime, and per-session transfer count; validate request fields, transfer ownership, offsets, and checksums.
- Route mode changes through the existing Bridge SetMode command. Classic writes and mode transitions continue through the same queue and queue guard. Terminal records a pending target and reports success only after a fresh authoritative Host snapshot confirms it. Rejection, timeout, reconnect, and conflicting Host state remain distinguishable; an unknown acknowledgement never triggers blind replay.
- Provide Graph read models from the Host-owned GraphRuntime and GraphDocShared. Reuse the Host Graph model, pin definitions, validation issues, and preset store.
- Implement the Terminal workspace as a Vue view over the Host Graph schema. The editable document is a per-Host local draft with an origin revision, local undo/redo, validation feedback, and safe import/export. Explicit Apply is the only path that can replace the Host graph.
- For Apply, upload large documents in bounded chunks, then enqueue a revision-conditional Host command. Compare and replace atomically against GraphRuntime's current revision when the Worker executes it, so discovery or local Host changes cannot be overwritten by a stale draft. A changed revision is rejected and reconciled; disconnects and timeouts preserve the draft and never replay an uncertain apply.
- Keep panic display read-only for remote clients. Graph editing and transfers must not delay panic handling or mode safety behavior.
- Use synthetic fixtures for tests. No automated test may activate a real Coyote output, flash firmware, or run unattended stimulation.

## Constraints and acceptance

- Source starts from live origin/main at 9b1e0ae02804a2a80833b70afb8f38d813480fcb, on a dedicated P5 feature branch and isolated worktree. Do not include newer upstream or primary-checkout changes.
- Preserve auth, rate limits, replay protection, session isolation, malformed-payload rejection, and current frame limits.
- Verify both WSS and RTC protocol handling, but do not claim WAN acceptance without a real remote-host test.
- Run focused Host/API/Terminal tests at each increment, followed by relevant full suites. GUI verification uses synthetic data only.
- Produce Windows x64 Release candidates only when implementation and checks support it; never install automatically.
- Publish sanitized reports and completed artifacts only to the user-specified P5 directory after applicable checks, with independent SHA-256 readback and checksum sidecars finalized last.
- Final report separates implemented and tested behavior, manual GUI acceptance, WAN acceptance, local-only limitations, blockers, exact commits, and verified artifacts.

## Implementation choices left to the builder

- Choose conservative document, chunk, and transfer bounds that remain comfortably below the existing frame limit after JSON encoding.
- Follow current Terminal Vue, Tauri command, session action, and local file interaction patterns.
- Prefer small API additions with default unsupported behavior for test or legacy backends.
- Keep each increment independently buildable and testable; stop before changing established Host safety behavior if a material product decision is uncovered.
