# 25. Tauri Terminal — Vue information architecture, state and UX

**Status:** PROPOSED v0.17; no code. **Source of functional coverage:** `docs/06-command-matrix.md` (37 remote operations) + `docs/15-data-model-and-operation-types.md` (approved DTO). Visual parity means **functional coverage**, not pixel-identical redesign of current `egui`.

## 1. Overall information architecture

**Pre-login:** Connect screen shows saved profile names and an add/edit form, Host address/IP/DDNS and port, Direct or RTC mode, first pairing/fingerprint comparison, username/password and optional `Remember credentials`. Store no server endpoints as defaults except direct TCP port `4318`; signaling/STUN/TURN fields start blank. In ICE mode provide a separate Rendezvous **Invite token** input (optional protected-store remember flag); never confuse it with Host password. Allow one selected profile per running Term instance, not multiple active Host sessions.

**Authenticated:** Sidebar/status bar with current Host label and version, transport, ONLINE/RECONNECTING/SESSION_BUSY, remote cap status, selected device + VRChat/BLE status and battery when supplied. Nine functional sections are reflected from `egui`:

| Section | Displays (Host DTO) | Operations (wire) | UI rules |
|---|---|---|---|
| **Status** | Host devices, global active device, BLE/VRChat link, channel current strength/output/frequency, live zone/activity, alarm phase | `devices.switch` | Exact actual values, 5–10Hz coalesced render; empty connected devices is valid |
| **Zones** | Channel A/B zone lists, discovered avatar zones, mode, scale and thresholds | `zones.add`, `zones.add_all_from_avatar`, `zones.remove`, `zones.set_mode`, `zones.set_scale`, `zones.set_threshold_min/max` | Index-based upstream semantics; update list before sending |
| **Channels** | Per channel four frequency segments, intensity, `limits.min/max`, aggregation, waveform preview | `channels.set_frequency`, `.set_intensity`, `.set_limit_min/max`, `.set_aggregation` | Preserve raw frequency unit; show 0..200 PowerLimits; cap permission affects only edits, not displayed values |
| **Tuning** | UKF q/r/alpha/beta/kappa, motion norms speed/acc/recoil | `tuning.set_ukf`, `.reset_ukf`, `.set_norms`, `.reset_norms` | Field-level editing with valid typed DTO; no client-side numerical algorithm |
| **Modulation** | Four freq and intensity modulation slots per channel; source/function/parameters | `modulation.apply`, `.clear_segment`, `.clear_channel` | Support all function variants incl. recursive Compose; max depth from schema |
| **Presets** | Current indexed waveform catalog, user presets, zone presets | `presets.refresh`, `.apply`, `.save_user`, `.delete_user`, `zone_presets.reload`, `.apply`, `.save`, `.delete` | Indices and order are **Host-owned**; catalog reload after writes |
| **Alarm** | Existing schedule/pattern/phase and all existing controls | `alarm.set_config`, `.set_channels`, `.test`, `.stop`, `.snooze` | Mirrors Host; does not invent extra Alarm safeguards or local timer |
| **Setup** | Host provided auto-save/alarm tab preference, transport status, read-only remote policy; Term profile + credential preferences | `prefs.set_auto_save`, `.set_alarm_tab_visible`, `config.save`, `config.load` | Only display/edit `prefs` if `snapshot.prefs` is advertised and actually present; otherwise show unavailable. Keep Host-only Remote Access enabled/cap/credentials controls **on Host**, never expose admin mutation remotely |
| **Logs** | Rolling sanitized log lines with cursor, filter/search, diagnostics/events | `logs.get` / `logs.append` | Cursor expiry triggers reset/page fetch; no large unbounded log render |

`presets.refresh` / `zone_presets.reload` can update catalogs asynchronously; controls show actual catalog loading/error. Updater operations (`CheckUpdate`, `StartUpdateDownload`, `SkipUpdateVersion`, `DismissUpdate`) are **not** remoted; they remain local egui only.

## 2. Status and realtime visualization

Use split **Control state** (slow-changing config) and **Telemetry** (status changing up to 10Hz):

- `hostState`: current authoritative snapshot and server epoch/seq, reset/replaced atomically on full snapshot; internal fields unchanged except projections for display.
- `telemetry`: last valid `status`/live zone data; throttled coalesced rendering. Do not drive a 60FPS animation just because worker updates every 50ms.
- `catalogs` and `logs`: lazy separate requests; not duplicated into whole-snapshot megablobs.
- `capabilities` + `remote_policy`: read-only server facts; supported operation list drives conditional visibility. A capability change should not crash the app; unknown supported read-only fields can be ignored per approved version contract.
- `connection`: friendly state, RTT/last heartbeat/transport info and deterministic errors, no secrets.

Prefer Vue 3 Composition API + Pinia (or equally small reactive store), `shallowRef` for large full snapshots, selectors/computed per view, debounced slider sends or explicit commit on release as appropriate to avoid commands-per-pixel. The Host gateway's approved burst limit 30 commands/s applies; never drop a final slider target silently. Client backpressure coalesces obsolete **unsubmitted** slider adjustments only, not already queued Host commands or discrete alarms/preset actions.

## 3. Reconciliation and UI authority

Example: Host local sets channel A max=120 while `Remote Maximum Strength A=70` is enabled. Terminal shows `120` as the true state. It cannot send 121; it may lower the value (subject to upstream `min` rules). Slider has an explicit edit affordance and policy explanation; **do not render fake `70` as actual strength**. Channel B respects its independent cap.

If local egui changes zone mapping while remote Zones editor is open, the next snapshot updates list order. Avoid a stale index action by warning or requesting refreshed catalog/snapshot; upstream is not atomic CAS, so do not promise perfect conflict prevention. Local and remote are equally able to mutate shared state (except Host-only settings). The last mutation **processed** by Worker wins; UI never fabricates a separate remote state of truth.

## 4. Command interaction design

- `user edit` -> local typed validate -> send via Rust IPC -> Rust API command -> `command.accepted/command.error`.
- `command.accepted queued:true`: show brief “sent” status; do **not** show “applied” or immediately overwrite authoritative values. Await subsequent snapshot to visually confirm new value when meaningful. Normalization by upstream is possible (e.g. requested max below existing min).
- `command.error`: show `code` + human-friendly description, keep current Host value and optionally retry only after user action.
- Reconnection: disable write widgets and show cached data clearly **stale**; when resumed, replace with **full fresh snapshot** before re-enabling writes.
- Unacknowledged mutation after link failure: **do not reissue as a new request** on reconnect. If needed, inspect fresh Host snapshot and let user decide. Use idempotency under approved request ID only for specifically supported replay, not for blind reexecution.

## 5. Connection pages and common failure UX

| Situation | Term behavior |
|---|---|
| Bad DNS, unreachable IP, closed port | Show network-specific error; retain editable host profile; retry is user-controlled |
| First-time Host identity | Show side-by-side fingerprint and pairing confirmation referencing Host's local display |
| Same host, new DDNS/public IP | Accept same pinned key, refresh resolved network address |
| Same hostname, changed Host key | Block auto-login; show re-pairing requirement and fingerprint mismatch |
| Password incorrect | Clear entered password from form unless user selected OS storage; show auth error |
| Other remote operator connected | `SESSION_BUSY`; do not attempt to kick them; offer user retry |
| Temporary Wi-Fi/VPN dropout | `RECONNECTING` with 30s Host-side reservation after detection; automatic ticket resume |
| 30s grace expired | `LOGIN_REQUIRED`; show manual login/reconnect; no silent claim of session ownership |
| Host kick / Remote OFF / password change revoke | `REVOKED`, stop auto reconnection, show explicit user login path |
| Host restart | Reconnect with verified Host identity then fresh authentication; volatile old session erased |
| TURN unavailable / signaling fails | RTC-specific error; direct connection may be offered but not silently trusted as equivalent identity |

## 6. UI preferences and release appearance

- Windows-first, compact desktop layout with a dark readable default and optional aesthetic theme. The user may decide later whether to reuse `egui` retro-green styling or create a new look; no design veto from client architecture.
- Keyboard usable for numeric inputs, accessible focus state, high DPI and minimum sensible window size; avoid layout shifts while telemetry updates.
- Loading states: `not connected`, `syncing`, `no hardware`, `no avatar`, `empty presets` distinguished clearly.
- Remembered profiles: display name, host, port, transport preference, fingerprint; credentials optional and OS-protected. Multiple saved profiles don't imply multiple simultaneously active sessions.
- Local window focus/minimize may reduce Vue render frequency. Network heartbeat stays alive while minimized. Closing the app (not merely hiding) logs out best effort; no mandatory tray/background component.
- UI text can be localizable in future; v1 architecture keeps strings separated in components, localization is not an implementation gate.

## 7. View-by-view functional verification

Create a fixture-driven checklist for each operation and field: actual snapshot rendering, success ACK vs final truth, validation failures, no-device case, local egui concurrent edit, remote cap ON/OFF with A/B different, disconnected and resumed state. Test Alarm as existing upstream commands using fixtures; do not develop new scheduling behavior.

Visual comparison should focus on **coverage**, not making the new frontend pixel-exact to current `egui`. User can decide final visual direction after component prototypes. No Codex UI redesign acceptance is implied yet.
