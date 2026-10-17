# ShockingVRC Terminal UI Parity Plan

## Constraints

- Use `architecture/v1.1/` and `architecture-addenda/v1.2-passwordless-pairing.md` as the normative references; do not edit `architecture/v1.1/`.
- Preserve the Terminal's left navigation and Passwordless flow. Do not change Host GUI, Engine, BLE/OSC/DSP/Alarm algorithms, Power Limits, Worker cadence, Remote Session architecture, identity/challenge-response, ICE/TURN, or the 37 Remote API operations.
- Keep all controls snapshot-backed and capability-gated. Separate draft values from Host-confirmed values; never write during render or on ordinary Snapshot receipt.
- UI changes remain uncommitted until the owner separately authorizes the UI Parity commit. Do not push, deploy, install the output, or run physical/internet acceptance.
- Keep the pre-existing `2026-10-16-passwordless-pairing.md`, `deliverables/`, and ignored build/runtime state intact.

## Implementation sequence

1. **Inventory and types** — map each requested section to current Snapshot, capability, catalog, and allowed-operation fields. Record exact Host ranges and modulation semantics. Identify missing DTO fields and implement typed projections without changing wire contracts.
2. **Shared Terminal theme and controls** — add compact graphite/dashed/striped tokens and shared Panel, Button, Slider, NumberInput, SegmentedControl, StatusIndicator, ProgressBar, Waveform, Chart, and LogViewer components. Add focused component/domain tests before behavior changes.
3. **Connection, Status, Channels, Tuning** — build compact responsive layouts from live Host state. Add local draft/commit handling, snapshot synchronization, command feedback, Host range handling, and measured kinematics without synthetic values.
4. **Zones and Presets** — rebuild mapping/calibration and searchable preset catalog views around existing APIs and data. Preserve ACK-vs-confirmed-state semantics; ensure presets do not write limits or zones.
5. **Modulation, Alarm, Setup, Logs** — implement a visual slot editor and exact local preview for supported Engine variants, including bounded recursive Compose. Keep advanced JSON, preserve Passwordless/Alarm behavior, and add filters/follow/hold/copy/clear controls backed by existing data.
6. **Visual and package verification** — capture real Terminal screenshots at the requested sections and inspect them at 1920x1080, 2560x1440, and 1366x768 where the environment permits. Run unit/type/build/Rust checks; package a new versioned NSIS x64 installer; record its exact path, byte size, and SHA-256. Report assisted acceptance gaps without claiming UI Parity PASS unless every required screen and interaction is locally verified.

## Verification gates

- Each behavior change follows test-first development: add a focused failing test, implement the smallest change, then rerun it.
- Check exact numeric ranges and Engine modulation semantics against current Rust sources before exposing controls or preview results.
- Inspect the final source diff and `git status`; ensure generated installer/build outputs, runtime data, secrets, and historical handoff artifacts remain excluded from Git.
- No UI Parity commit or push in this plan.
