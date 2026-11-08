# P5 Increment 2 — Editable Remote Graph Workspace

## Goal

Turn the read-only Terminal Graph page into an editable local draft workspace that uses the Host's existing Graph model, node catalog, pins, grouping rules, presets, and validation. A draft remains local until the user explicitly applies it to Host.

## Design decisions

1. **Reuse the existing Rust Graph model.** Add `shocking_vrc_core` as a Terminal Tauri dependency for Graph serialization, node catalog descriptors, pin definitions, graph edits, preset instantiation, and compile validation. Terminal will not instantiate or run `GraphEvaluator`; Host remains the only graph evaluator and output authority.
2. **Keep drafts separate from Host state.** Opening a workspace clones the current Host document into a Terminal draft. Edits, preset application, undo/redo, import, and local save affect only that draft. A Host revision change marks the draft stale and never replaces it silently.
3. **Keep file operations local to Terminal.** Save/reload uses a bounded per-profile draft file under Terminal app data with atomic replacement. Import/export uses local browser file input/download semantics and the same Rust parser and size limits. No remote file access is added.
4. **Load preset contents through the authenticated Host session.** Keep the existing summary catalog and add a bounded, session-bound Graph preset transfer for the selected preset. Preset application uses the canonical Rust `GraphPreset` methods against the local draft.
5. **Apply is explicit and transactional.** Add a confirmation step. Apply sends a bounded Graph upload with its base Host revision. The Host validates and serializes the request through the existing Bridge worker and `GraphRuntime::set_graph`; the worker compares the base revision immediately before replacement. The Terminal reports success only after a newer Host document matches the submitted canonical graph. Timeout stays unknown and is never retried automatically.
6. **Preserve established Host output behavior.** The existing GraphRuntime replacement path handles ramping and panic behavior. The confirmation warns that applying while Graph Mode is active changes the running graph. No panic-release operation or second evaluator is introduced.
7. **Implement the canvas with existing Vue/CSS/SVG primitives.** Avoid adding a graph library dependency. Support pan, zoom, framing, node selection and movement, typed pin connections, node add/delete, input defaults, parameter inspection, duplication, grouping, undo/redo, and read-only Host revision/panic/validation status.

## Data flow

```text
Host Graph snapshot ──read──> Terminal draft ──local edit/save/import/preset──> Terminal draft
                                      │
                                      └── explicit Apply + base revision
                                               ↓
                                  authenticated bounded upload
                                               ↓
                         Host validation → serialized worker → GraphRuntime
                                               ↓
                                  fresh Host document confirmation
```

## Validation and safety boundaries

- Reject Graph payloads larger than the existing 4 MiB document cap and JSON nesting deeper than the Graph model permits before parsing.
- Validate Graph version, node kinds, references, pin direction/keys, cycles, nesting, finite numeric values, and compiled Graph issues at the Host boundary. Terminal validation is a convenience only.
- Use session-bound, expiring upload and preset transfers; preserve authentication, replay protection, rate limits, frame limits, and session isolation.
- Reject apply if the base revision changed before the Host worker executes. Keep the Terminal draft and show the current Host revision for explicit reload or manual recovery; never auto-retry.
- Emergency-stop and panic controls remain independent of Graph transfer traffic. No test activates physical outputs.

## Acceptance

- Synthetic UI tests cover graph add/move/select, typed connections, disconnection, inspector edits, defaults, grouping, duplication, undo/redo, preset application, local save/reload, import/export, stale Host state, and explicit apply confirmation.
- Rust tests cover canonical schema descriptors, bounded draft files, validation, bounded preset reads and Graph uploads, revision conflict at worker execution, and unchanged Host evaluator ownership.
- Production-path synthetic WSS and DataChannel tests prove apply request ordering and fresh state confirmation without hardware.
- Run the relevant full Rust, Terminal, Node, and source-security checks. Manual GUI and WAN acceptance remain separate evidence.
