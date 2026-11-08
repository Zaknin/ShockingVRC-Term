# ShockingVRC P5 Increment 1 — Verification Report

Date: 2026-11-08
Repository: `Zaknin/ShockingVRC-Term`
Branch: `codex/p5-remote-graph-parity`
Baseline: `9b1e0ae02804a2a80833b70afb8f38d813480fcb`

## Implemented

Increment 1 adds an authenticated, capability-negotiated Graph API over the existing Remote API transports. Host reads expose the current graph document, revision, validation issues, editor metadata, and preset summaries. Graph documents transfer in bounded chunks with transfer/session binding and SHA-256 integrity checks; the existing 256 KiB API frame limit remains unchanged.

Terminal receives Host `engine_mode`, can request Classic/Graph transitions, and waits for a newer authoritative Host snapshot to confirm the requested state. A queued acknowledgement is shown as pending, while timeout or reconnect remains an unknown outcome and is not blindly replayed. The read-only Graph workspace displays nodes, connections, revision, validation, panic state, and preset summaries. It exposes no Graph editing or panic-release action. Host GraphRuntime remains the evaluator and output authority.

Older Hosts continue to work without Graph capability features. Tests cover unsupported mode requests, legacy capability fallback, Classic-write queue ordering, malformed or oversized transfers, session binding, and mode reconciliation.

## Transport evidence

- Production Host WebSocket router fixture: authenticated identity and device pairing, capability negotiation, document start/chunk retrieval, checksum and length verification, preset catalog, queued Graph mode request, then a newer snapshot confirming Graph mode.
- Production WebRTC DataChannel acceptance fixture: authenticated capability and Graph reads, document integrity, presets, queued Graph mode request, and fresh-snapshot confirmation. The fixture also confirms the existing safe modulation operation still reaches Host.
- Both fixtures use local synthetic test backends. These results establish protocol behavior over the production router/session paths; they do not establish Internet/WAN acceptance or behavior on separate physical machines.

## Commits

| Commit | Change |
| --- | --- |
| `eb53d8f6b11fb72fcfe86f4aa644d6447440059c` | Remote Graph protocol and mode contract |
| `b989eb92ae00d6d6c6d57c612f70583a609a45fb` | Host adapter Graph state and mode mapping |
| `4147fcc96b88cad5cbb9b649389cd3fbc94ff296` | Authenticated bounded Graph reads |
| `3cbc3c0c62ef3703f03fa3be51490540df11178f` | Terminal Graph transport and mode requests |
| `7fea6db7c73236340468decb167e6edc6ebac78c` | Read-only Terminal Graph workspace |
| `bc40cb783e349d77ef623b61c5cda2d09ec17bb0` | Production WSS and WebRTC Graph acceptance fixtures |

## Verification

| Command | Result |
| --- | --- |
| `cargo fmt --all -- --check` | Passed |
| `cargo clippy --workspace --all-targets --locked` | Passed; non-fatal Clippy warnings remain |
| `cargo test --workspace --locked` | Passed; includes production WebSocket Graph coverage and the local two-process WebRTC suite |
| `npm test` | Passed, 163/163 |
| `npm run build` | Passed (`vue-tsc --noEmit` and Vite production build) |
| `cargo fmt --manifest-path src-terminal/src-tauri/Cargo.toml -- --check` | Passed |
| `cargo test --locked --manifest-path src-terminal/src-tauri/Cargo.toml` | Passed, 89 passed and 1 existing loopback TURN fixture ignored |
| `npm audit --audit-level=low` | Passed, 0 vulnerabilities |
| `cargo audit` | No vulnerabilities; one allowed unmaintained-crate warning for `paste` (`RUSTSEC-2024-0436`) |
| Baseline-to-worktree common credential-pattern scan | No matches |
| `git diff --check` | Passed |

The repository GitHub Actions workflow runs push jobs only for `main`, `egui`, `publication/host-terminal`, and `integration/**`, plus pull requests. It does not start hosted CI for this `codex/*` branch push. The workflow's Rust, Node, build, formatting, and Terminal test commands were run locally on Windows and passed. No pull request was opened, so hosted CI remains unobserved.

## Acceptance boundaries and remaining work

- Manual GUI acceptance on an installed Host and Terminal was not performed.
- WAN Graph reads and mode transitions between separate machines were not exercised for this increment.
- Graph editing, applying or saving drafts, undo/redo, and concurrent edit reconciliation are later increments and are not implemented here.
- Tests use synthetic fixtures only. No real device output, firmware flashing, unattended stimulation, or remote panic release was performed or added.
- No Release candidate was produced or installed.

This report records Increment 1's local implementation and automated transport evidence. It does not claim full remote Graph editing parity.
