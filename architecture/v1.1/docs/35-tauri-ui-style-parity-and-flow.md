# 35. Tauri Terminal: egui-aligned design and interaction specification

**Decision accepted 2026-10-15:** new Tauri UI should be **stylistically close to existing egui**, not a visually unrelated overhaul. Applies to remote-only Windows Tauri Terminal. Core uses Vue 3 + TypeScript; network/session functions remain in Rust.

## Visual character

- Dark/charcoal surfaces with restrained green/terminal accents in the spirit of current ShockingVRC egui; clear high-contrast text and focus states. Do **not** copy source pixels/screenshots or assert exact colors before inspection of running egui UI.
- Technical control panel feeling: subtle panels and group headers, concise parameter labels, same section structure. Modern Vue components can be more scalable but should feel familiar to Nightmare and AmigoFPS.
- Avoid excessive background animation, glow, shadows, full-screen particle effects and expensive redraws. Visual similarity is primary, performance is measured later.
- Navigation on left or top according to window size, persistent transport/session status in header, channel A/B clearly differentiated; title/version shown.
- Windows scaling/DPI and keyboard focus must work; no browser popups for core settings.

## Screens: functional parity with current egui

| Section | Key widgets | Commands/sources |
|---|---|---|
| **Connect** (Terminal-only) | Saved Host profiles, direct address/DDNS, signal server, paired fingerprint, login, path diagnostics | `hello`, auth/resume, connection states |
| **Status** | VRChat OSC status, BLE devices/battery, selected device, live A/B levels | Snapshot + `devices.switch` |
| **Zones** | A/B zone inventory, avatar zones, add/remove, mode, scale, thresholds; zone sets | `zones.*`, catalogs |
| **Channels** | A/B four waveform segments, frequency, per-segment Intensity, min/max Power Limits, aggregation | `channels.*` |
| **Tuning** | UKF parameters and motion scaling with reset | `tuning.*` |
| **Modulation** | Frequency/intensity modulation per channel + segment, function/source and clamps | `modulation.*` |
| **Presets** | Default/user presets, apply/save/delete, zone preset workflows | `presets.*`, `zone_presets.*` |
| **Alarm** | Existing schedule/pattern/actions | `alarm.*`, **no new alarm semantics** |
| **Setup / Settings** | Auto-save and already allowed GUI preferences; remote policies read-only | `settings.*`, capabilities/policy |
| **Logs** | Bounded display, cursor/paging, copy sanitized diagnostic info | `logs.get` / logs updates |

Existing egui contains nine sections; **Connect** is an additional Terminal connection view, not removal of any of the original nine. Catalog and alarm visibility behavior follows approved API and original Host state. Updater commands remain Host-local; Term does not show buttons pretending to invoke them.

## User journey

1. Open Term → `Connect`; select or add host profile. No automatic public endpoint or Moscow preset.
2. Address by local IP, DNS/DDNS, VPN or own rendezvous. Verify paired Host identity before password input is used. Explicit pairing via fingerprint/QR displayed by Host.
3. Login with username/password; optional OS credential store. Fetch full Snapshot and capabilities; navigate to Status.
4. Drag a channel slider: Term sends single validated command, shows pending/accepted state; **actual** slider value reconciles to Host Snapshot and cannot be fabricated from ACK alone. Coalescing for drag gestures must not reorder distinct user operations.
5. Local egui edits config concurrently: latest processed Host state appears in Term; a stale form indicates actual/pending vs edited state clearly.
6. Brief network loss → connection banner and disabled writes, 5/15/30 timer state → resume and full resync. Host kick → `Revoked`, no silent auto-login.
7. Normal `Logout` → release slot; closing the app attempts logout best-effort and exits (no background tray agent in v1).

## Power cap UI details

Host-only cap values are visible as read-only policy; when `enabled=false`, remote Max slider range 0–200. When enabled, **future writes** to A or B Power Limits are restricted by channel cap, but readbacks and status MUST continue displaying larger Host-origin values unmodified. Do not snap the displayed power down to cap; distinguish actual and maximum remote-settable bound.

## Component architecture

`ControlPanel`, `ChannelSelector`, `PowerSlider`, `SegmentEditor`, `ZoneTable`, `ModulationEditor`, `PresetPicker`, `AlarmPanel`, `HealthChip`, `PathBadge`, `SessionBanner`, `ErrorToast`, `SnapshotStaleOverlay`. Types/schema generated or checked against approved JSON Schema; all sensitive session secrets remain in Rust, not Vue stores/localStorage.

## Visual validation checklist

- Side-by-side comparison against running `egui` at fixed DPI; consistency of sections, visual hierarchy and green/charcoal design.
- Screenshots on Windows 100% and 150% scaling; 900×620 equivalent UI usable, smaller windows scroll without hiding key controls.
- All 37 approved operations reachable by UI, no fake disabled/hidden gaps.
- No apparent success immediately upon `command.accepted` when Host state did not change.
- Minimized WebView rendering idle measured; connection/heartbeat stays alive as approved.
