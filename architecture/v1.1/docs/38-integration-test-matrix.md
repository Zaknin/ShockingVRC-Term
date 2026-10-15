# 38. Integration and regression acceptance matrix

**Status:** COMPLETE PLANNED TEST MATRIX, **NOT EXECUTED**. Method of future architecture audit is reserved to owner's forthcoming instructions. These are pragmatic development/acceptance checks, not an imposed security audit or destructive stress programme.

## Levels

`L0` JSON Schema/static validation; `L1` unit tests; `L2` loopback Host fixture/Term; `L3` isolated Linux rendezvous + coturn and two Windows clients; `L4` approved friends-only actual DG-LAB/VRChat controlled test. **L4 requires explicit operational agreement; no automated destructive device tests.**

## Scenarios

| ID | Level | Scenario | Expected observable result |
|---|---|---|---|
| BASE-01 | L1 | `cargo test --workspace` upstream baseline | Existing 137 tests still pass; report new total separately |
| BOOT-01 | L2 | Remote Access OFF default | No remote listener/registration; current GUI and engine unchanged |
| BOOT-02 | L2 | Enable listener + occupied TCP port | Local Host survives; clear listener error |
| API-01 | L0/L2 | All 37 operation DTOs and invalid args | Exact mapping or typed error; updater not remote |
| API-02 | L2 | Enqueue ACK and later Snapshot | ACK not mislabelled as hardware-applied |
| API-03 | L2 | Full REST companion via same session | REST+WSS shares one slot, no second operator |
| API-04 | L2 | Snapshot 5–10Hz coalescing | ≤10 Hz on wire; existing 50ms Worker untouched |
| SYNC-01 | L2/L4 | Host changes channel A power | Terminal displays true new value, including above remote cap |
| SYNC-02 | L2/L4 | Term modifies zone/preset/UKF/Alarm controls | Host updates authoritative state; no altered semantics |
| CAP-01 | L2 | Caps OFF default A/B | Both write `0..200` per native rules |
| CAP-02 | L2 | Caps ON A70/B120 | Remote Power Limits > cap rejected per channel; Host local unrestricted |
| CAP-03 | L2 | Other remote commands under cap | Intensity/zone/modulation/preset/Alarm remain unchanged |
| CAP-04 | L2 | `config.load` with Host-local values above cap | Approved Option A preserved, no indirect new cap |
| SESS-01 | L2 | Two simultaneous terminals | One admitted, next `SESSION_BUSY` |
| SESS-02 | L2 | Heartbeat absent ~15s then resume <30s | Replaced transport generation, full Snapshot |
| SESS-03 | L2 | Resume after grace 30s | Old ticket rejected; slot free |
| SESS-04 | L2 | Replayed `auth.resume` lost reply | Same `request_id` returns cached result within 15s window |
| SESS-05 | L2 | Host kick or Remote Access OFF | Current session revoked; no automatic relogin; local engine unaffected |
| AUTH-01 | L2 | Wrong password, expired bearer, changed Host password | Typed denial, no Host command accepted |
| AUTH-02 | L2 | Fake identity during Direct / RTC | No credentials sent before verified Host proof |
| DIRECT-01 | L3 | LAN/TCP TLS/WSS | Connect/read/write with Host pin |
| DIRECT-02 | L3 | VPN hostname / DDNS IP changes | Re-resolve hostname, same Host key trusted |
| PORT-01 | L3 | UPnP supported / unsupported / lease timeout | Mapping optional; manual path works without it |
| SIGNAL-01 | L3 | Own rendezvous + Host registration + SDP exchange | Separate signaling does not execute Remote API commands |
| ICE-01 | L3 | Peer on same LAN | Verified RTC host pair where possible |
| ICE-02 | L3 | Across NAT with STUN | Verified srflx direct pair if NAT permits; no guarantee |
| TURN-01 | L3 | Force relay by disabling direct candidates in test harness | Actual relay pair and same application JSON frame semantics |
| TURN-02 | L3 | Invalid/expired coturn credentials | Clear `turn_auth_failed`, Direct unaffected |
| TURN-03 | L3 | CGNAT-like isolated networks | TURN works if public relay reachable |
| DIAG-01 | L2/L3 | Disconnected/rendezvous dead/credentials missing | Accurate state and actionable visible diagnostics |
| UI-01 | L2 | All nine original egui sections | Functional parity of 37 operations; familiar dark green styling |
| UI-02 | L2 | Windows 100/150% DPI, resize, suspend/minimize | Correct layout and heartbeat without excessive render |
| DIST-01 | L3 | Windows EXE/NSIS install, launch, uninstall | Host unaffected, Term data separated |
| FAIL-01 | L2/L3 | Host/Rendezvous/coturn restarts | Controlled retry, no orphan session or corrupted Host config |
| LICENSE-01 | L0 | Attribution/LICENSE files in release | GPL-3.0 upstream terms respected |

## Special negative tests (software only)

Send invalid frame sizes, duplicate JSON keys, unsupported enum values, wrong `request_id`, unknown `op`, stale generation command, synthetic malformed SDP and truncated snapshots. Use fixture/mock Host data. These tests must not call physical device APIs or change router/firewall rules.

## Evidence expectation per phase

Record exact git commit/branch, Windows/Rust/Node versions, fixture IDs, commands/test invocation, pass/fail totals, diffs, SHA-256, selected network route evidence without secrets, and unresolved failures. Never substitute `L0` schema PASS for WebRTC connected PASS, or installed executable success for device integration PASS.

## Exit rule

Each milestone has its own gate in `39-implementation-roadmap.md`; failed gate blocks that milestone's completion, not unrelated working Host operation. The owner will provide audit policy later; this matrix is **project validation**, not audit terms.
