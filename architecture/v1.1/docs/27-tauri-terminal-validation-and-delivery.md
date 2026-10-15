# 27. Tauri Remote Terminal — validation, build and delivery plan

**Status:** PROPOSED v0.17. This is a practical implementation acceptance plan, not an audit protocol. The owner will separately supply audit instructions when the complete document set is ready. Nothing in this document mandates destructive testing, production deployment, public exposure, extra device safety logic or invasive network experiments.

## 1. Baseline and boundaries

- Starting source: `Zaknin/ShockingVRC-Term`, `feature/remote-access`, base commit `e076bc2ede4726a976c60613031f48c0bc31e0b9`; `cargo test --workspace` 137/137 PASS as baseline evidence on Windows.
- Host API v1 design: docs 14–19, all 37 wire operations and DTO. Host implementation architecture: docs 20–23, currently proposed only, **not approved by implication**.
- Project order agreed: Host Remote server → Tauri remote client → egui FPS limiter → self-hosted ICE/TURN → end-to-end scenarios → optional local Tauri replacement.
- Hardware control algorithms unchanged. Tests can use stub backend and pre-recorded fixtures without any actual device outputs.

## 2. Component-level tests (Term)

| Target | Example test | Expected result |
|---|---|---|
| JSON models | Verify generated TS/Rust types against approved schema and sample messages | No drift or undocumented fields |
| IPC facade | UI invoke `submit_operation` maps to exact typed Rust call | No direct Vue network/session access |
| Profiles | Add/edit/delete profile, IP/IPv6/DDNS, validation and RTC empty defaults | Deterministic persistence, no embedded server |
| Credential storage | Remember OFF/ON, remove, app restart | Plain config free of passwords/tickets |
| Pairing | Unknown Host prompts, matched pin accepted, altered key blocked | No credentials sent to mismatched key |
| Session | Login, token refresh, logout, kick/revoked, restart | One active connection and correct state transition |
| Resume | 5s ping, ~15s detection, 30s Host grace, duplicate resume ACK | Same session, only one generation |
| Snapshot | Connect full sync, <=10Hz update, seq gap resync | UI shows actual Host state, no stale fake truth |
| Commands | Each of 37 ops against fake Host contract | Correct args and ACK handling |
| `Command.accepted` | Enqueue response received without corresponding immediate state change | UI reports queued, **not applied** |
| Catalog index | List reordered locally / concurrently | UI refreshes/flags stale selection; no CAS claim |
| Remote A/B cap | Disabled, A=70/B=120 enabled, Host local >cap | Policy enforced by Host, Term displays actual unclipped value |
| Alarm | Existing `alarm.*` calls with stub state | No invented timer/power constraints |
| Logs | Empty/normal/cursor-expired/200-line page | Proper pagination and bounded rendering |
| Window lifecycle | Minimize, focus, reload, quit | No duplicate login, heartbeat continues while minimized |

## 3. Functional coverage map

Create table-driven tests for all 37 operations in `docs/06-command-matrix.md`:

- `devices`: 1 operation.
- `zones`: 7 operations.
- `channels`: 5 operations.
- `config`: 2 operations.
- `tuning`: 4 operations.
- `modulation`: 3 operations.
- `presets`: 4 operations.
- `zone_presets`: 4 operations.
- `alarm`: 5 operations.
- `prefs`: 2 operations.

Total: **37**. Four GUI local-only updater operations excluded deliberately. The tests should be anchored to the approved machine-readable schemas rather than duplicating numbers in an implementation-only enum.

## 4. Transport and network scenarios

**M2 (direct client):** Windows Host in normal egui with Remote ON; Term can reach it via local IP, VPN address or user-configured DDNS; TLS pin verified; invalid password, second client, Host kick, timeout, reconnect and local UI edits. Manual WAN forwarding or UPnP tested **when specifically configured**; no assumption of a business router or of a cloud relay.

**M4 (ICE/TURN):** bring a user-owned rendezvous and TURN config; test direct ICE candidate, ICE timeout, forced TURN relay, Host peer identity match/mismatch, stale SDP/fingerprint, RTC reconnect and transition between direct WSS and RTC without creating a second active session. The private Moscow server with OpenShock may host signaling/coturn as a **separate optional instance**, but no address/secret may be hardcoded in distributable defaults.

**M5 (integration):** compare Host local egui vs Term for every section, allow ordinary live values and changes in both directions; check 5/15/30 timing with packet drop simulation, not destructive router actions. Confirm Term close/logout has no effect on local Host BLE/OSC/Alarm. Mixed connectivity failures should be normal visible errors, not worker crashes.

## 5. Windows build and packaging

Development prerequisites: Rust/MSVC toolchain, Node.js with package manager, Tauri 2 dependencies and Microsoft Edge WebView2 runtime. Target OS: Windows 11 primary, optional Windows 10 if deliberately tested. Recommended release packaging: `.exe`-based NSIS installer and optionally portable executable if proven compatible with chosen dependencies. A signed installer is optional, not a required blocker for personal testing. No mandatory enterprise telemetry, service account or auto-updater.

The completed Term should not start a Node.js server at runtime, should not install OSC/BLE hardware components, and should not require its user to install a coding toolchain. Test fresh Windows startup, running from non-admin account, DPI modes and simple uninstall/reinstall of Term while Host continues running. Avoid measuring memory use using only debug builds: compare release builds with realistic telemetry (10Hz) and minimized window behavior.

## 6. Practical acceptance criteria

1. User can add profile, pair by matching Host fingerprint, login and see Host state without manually editing JSON.
2. At most one remote operator; `SESSION_BUSY` is handled clearly.
3. All nine original GUI sections and 37 supported operations are reachable and tested; four updater operations absent.
4. Local and remote UI converge on the same Host Snapshot; `command.accepted` never masquerades as hardware-applied.
5. Caps A/B and Host-only setting ownership behave exactly as approved, disabled by default.
6. Connection loss triggers resume; Host kick/revoke does not auto-login; saving credentials optional.
7. Local Host engine remains functional independently of Term network errors.
8. Direct IP/DDNS networking works with no signaling/STUN/TURN configured; later M4 makes RTC operational with user-specified infrastructure.
9. Windows artifact launches without development dependencies on target machine; build and test logs capture versions/commit.
10. The project owner decides when to audit and provides the audit method; this plan does not run an audit or authorize infrastructure changes.

## 7. Evidence packaging (after implementation, not now)

Save build logs, contract fixtures, integration outcomes, commit SHAs, release artifact hashes, and known limitations. Canonical project path: `\\\<evidence-share>\Torrents\Codex\ShockingVRC-Term\...`; independently verify read-back SHA-256 and write a sidecar last. Downstream Google Drive mirror is optional. **v0.17 architecture ZIP is locally produced only**; it is not published to SMB/GitHub and carries no implementation evidence.

Do not conflate JSON-Schema validation of a written spec with runtime API conformance. No current Host/Term implementation is claimed to exist.
