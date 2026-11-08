# P5.3 Graph Context Menu and Windows RC Plan

## Scope

Complete the existing Terminal Graph editor's right-click Add Node flow, simplify visible mode status, correct the obsolete Graph banner, validate existing regressions, and publish Windows x64 release candidates from one verified commit. Keep protocol, Host authority, Apply, revision checks, transports, authentication, evaluation, and panic behavior unchanged.

## Implementation

1. Add deterministic canvas coordinate conversion for the SVG `viewBox` with centered aspect-ratio letterboxing; use it for context-menu placement, drag/pan deltas, and wheel zoom so display scaling and window proportions are handled consistently.
2. Add a blank-canvas-only context menu sourced from `nodeCatalog`, with category grouping, search, keyboard navigation, focus, Escape/outside dismissal, viewport clamping, and local draft/undo edits. Keep SVG context-menu suppression scoped to the canvas.
3. Replace ordinary mode messages with the required concise strings while retaining state, revision, acknowledgement, and timeout metadata and existing snapshot-confirmation rules. Remove the idle fallback and correct the Graph banner.
4. Add tests for all fourteen required behavior groups, including menu boundaries, coordinate mapping, draft locality/undo-redo, preserved interactions, mode-state distinctions, and banner text.

## Verification and delivery

1. Run focused Node tests, then all Terminal Node tests, Host and Terminal Rust tests, frontend production build, formatting, and Clippy; preserve P1 lifecycle and Apply/revision-conflict regressions.
2. Build Host and Terminal x64 artifacts from the same final source commit using existing build caches. Package executables, NSIS installer, required Classic presets, Graph preset inventory/assets, runtime instructions, manual checklist, manifest, and SHA-256 records.
3. Commit implementation separately from verification/package evidence, push only `codex/p5-remote-graph-parity`, and verify remote HEAD/tree equality.
4. Publish through `.part` files to the canonical SMB destination, independently read back and hash the bytes, atomically finalize, and write sidecars last. Keep GUI acceptance pending if safe isolated WebView2 interaction cannot be run; report live WAN Apply separately.

## Self-review

- Host catalog remains the only source of node kinds; no fake Paste action is added.
- Add Node reuses the existing graph model mutation, emits only `updateDraft`, and remains local until explicit Apply.
- The menu opens only on the canvas background; context-menu suppression is attached only to the SVG.
- Existing text fields, drag, pan, zoom, pins, multi-select, inspector, stale/apply/panic notices, and mode confirmation predicates remain covered.
- No device, installation, profile, main-branch, release, or deployment changes are included.
- Automated DOM tests will not be represented as proof of native WebView2 context-menu behavior.
