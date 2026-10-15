# 34. Existing egui: Remote Access settings UI

**Status:** v1.0 detailed UX PROPOSAL; preserve current egui look and local engine behavior. Host is the only editor of access settings, username/password, pins and A/B caps. Remote API exposes read-only policy where approved.

## Location and layout

Extend current `Setup` screen with a clearly named `Remote Access` panel or sub-tab. Keep existing controls, egui color palette, spacing, keyboard navigation and layout. Do not remove/update current existing sections to make room. The new panel groups:

1. **Service:** master `Enable Remote Access` (default OFF); proposed TCP listen port `4318`; observed listener state, optional binding interface; status/error if port occupied; local copy address.
2. **Credentials:** username and set/change password, no password ever displayed in status; password change revokes existing remote access per accepted ADR.
3. **Peer pairing:** stable Host ID/public fingerprint, copy/QR, regenerate only with explicit action and clear re-pair consequences.
4. **Remote session:** `Active`, `Reserved`, `Disconnected` state; client profile or anonymous connection label; last heartbeat, path (WSS/RTC); `Disconnect Terminal` immediately revokes session and blocks auto-login.
5. **Remote Maximum Strength:** master toggle OFF by default; independent `Channel A` and `Channel B` values 0–200. When OFF, numeric values are saved but ignored. Only remote `Power Limits Min/Max` writes are checked, NOT Intensity, Alarm, Zones or Presets. Local Host never capped and Term must show actual values even above cap.
6. **Direct networking:** manual public address/DDNS display/input as desired, listener port, optional UPnP/NAT-PMP/PCP mapping toggle, mapping result/lease, manual port-forward guidance; mapping success != verified WAN reachability.
7. **Traversal:** explicit `Enable ICE` and independently editable Rendezvous URL, STUN URI list and TURN server list; **all blank by default**. Add **Host registration key** field (secret input, optional OS-protected persistence), manually provisioned by the chosen rendezvous operator, sent as `register.authorization` only. Never prefill it with a project-wide value. Show missing config if traversal enabled without rendezvous/registration key. The rendezvous **invite token is not the Host login password**; operator passes it privately to the Terminal user.
8. **Diagnostics:** selected transport and connectivity errors; a bounded copy/export of diagnostic metadata (not tokens/keys), and local-only button to test direct listener without device interaction.

## Remote-limits example

Host sets `enabled=true`, `A=70`, `B=120`. Term sees Host actual A=150 and B=110 if those are local values. Term can request `A.max=70` or less; `A.max=71` yields approved remote policy error; Host itself may keep A=150. B cap independently applies to B, and never clamps actual displayed status. Existing engine not modified.

## State and ownership

- Remote controller start/stop is async under existing Tokio runtime; egui button does not block render thread.
- Only a successful start shows `Listening`; port bind failure shows `Error` and keeps previous Host engine alive.
- Check and preserve Host-only settings across program restart; when `enabled=false`, no public listener or ICE registration starts.
- UI stage performs basic validation; Host `CommandPolicy` is authoritative for all remote methods, including REST.
- Avoid mixing device `active_device` selection with remote server settings; single existing active selection stays shared across Host/Term.

## Edge cases and snapshots

| Case | Expected display/action |
|---|---|
| Host local changes Power A during remote session | Tauri receives actual new value in Snapshot |
| Remote slider attempts above A cap | Clear `REMOTE_LIMIT_EXCEEDED` feedback, actual value stays visible |
| Remote disappears | `Reserved` then `Free` after approved heartbeat/recovery timing |
| Host clicks kick | Session revoked instantly; local processing untouched |
| Router lacks UPnP | Show manual forwarding route; local/VPN still work |
| Wrong rendezvous URL | Direct listener remains working; errors confined to traversal |
| Exit Host | Remote listener and owned port leases cleaned up before runtime shutdown |

## Acceptance

Visual fixture screens at 900×620 minimum existing egui window and larger layouts, tests for persistence defaults, controls never touch `Alarm`, normal 137/137 baseline stays green. No performance claims without actual measurements. No need to rebuild the whole local GUI in Tauri for this milestone.
