# 11. Remote session, live synchronization and Host-only Power Limits cap — v0.12 DRAFT

**Source:** accepted user decisions 2026-10-15; fork `egui` @ `e076bc2`. **Power policy:** **Option A ACCEPTED**. This document is architecture only; nothing has been implemented.

## Accepted: one active remote operator

- Maximum **one remote controlling session per Host**, across both direct HTTPS/WSS and ICE/WebRTC DataChannel. A simultaneous local egui session is allowed and does not occupy the remote slot.
- An unrelated second remote client gets `SESSION_BUSY`; it never automatically evicts the current client. Host GUI can explicitly revoke the active remote session.
- Explicit remote logout or turning Remote Access OFF frees the slot; a lost connection temporarily reserves it for the original authenticated session. **30 seconds after detected connection loss is an accepted reconnect grace (ADR-022)**; the accepted timing, resume ticket and fencing rules (ADR-022–027) are in `12-session-lifecycle-recovery.md`. Reconnect must never create two concurrent writers. Restart invalidates sessions.
- Remote disconnect/logout does **not** stop, reduce, or reset existing BLE, waveform, alarm or engine behavior.

## Accepted: live bidirectional synchronization

- Host `Bridge::snapshot` and other Host `Shared` state are authoritative; local egui and remote Terminal are views/controllers of **one** Host state and one shared `active_device`.
- The **last command processed by Host Worker** determines effective configuration. Do not treat the last packet sent or last UI slider motion as authoritative.
- Local egui edits are promptly visible on Terminal; Terminal edits are promptly visible on local egui. Full authoritative snapshot on connection and reconnect, then updates; network latency means *near real-time*, not literal zero delay.
- `Snapshot.revision` increments every Host publication (50 ms tick): it **does not** prove a specific command applied and is not a separate config generation. `command.accepted` denotes enqueue, not applied/physical output.
- Terminal must always render the **actual Host config and status**, including levels above the remote-only cap. No read-side clipping.

## ACCEPTED: Option A — Host-only remote Power Limits edit cap

**Purpose:** restrict what a remote operator may **write to `Power Limits`**, not restrict physical device output. This is a permission rule in the new Host remote API, not a new engine governor.

- UI working label: `Remote Maximum Strength`. Suggested Host-only fields: `remote_strength.enabled` and `remote_strength.max_by_channel.A/B` (not upstream fields). Stored only in Host Remote Access preferences; **must not be written to `CliConfig`**.
- **ADR-021 ACCEPTED: independent numeric caps for channels A and B**. Example: `A=70`, `B=120`. Changing A's cap never changes B's cap. The existing agreed master `enabled` switch controls enforcement of both values; independent per-channel enable switches are **not specified** in this decision.
- Host-local egui alone can edit/enable/disable the policy. Remote Terminal can only read effective policy. Do not expose a remote `set_cap`/`disable_cap` operation through either transport.
- Each channel has its own Host-configured cap in the upstream `0..200` scale. On a remote `channels.set_limit_min/max` command, choose **only the cap corresponding to the requested channel**. The cap for A must not be read when handling a B command, and vice versa.
- **First-launch defaults:** `enabled=false`; cap A and cap B are unset until entered locally. Host UI must not enable the master switch until **both numeric values have been explicitly configured**. No undocumented numeric default is applied.
- Example: if enabled with A=70 and B=120, remote `A.max=71` and `A.min=71` are rejected, while `B.max=100` is allowed and `B.max=121` is rejected. Check requests at the Host adapter **before enqueue**, with `REMOTE_LIMIT_EXCEEDED`. Requests at or below the selected channel's cap follow existing upstream PowerLimits logic and argument validation.
- This rule is about **requested values**, not whether an edit is mathematically an increase versus the old value. A remote client may **not** set 80 even if the local Host has already set 120. It can set a new value of 70 or less.
- Remote can still see `A.limits.max=120` when Host set A to 120, with separate display `Remote A limit 70` (B has its own cap). It **does not auto-clamp** Host values or override local changes.
- The existing `PowerLimits::new(min, max)` can **raise** a requested maximum to the current minimum. Example: Host A.min=80 / A.max=120, with A cap 70, remote asks for A.max=70; upstream would normalize A.max to 80. API must **reject rather than falsely acknowledge** this attempted write (e.g. `INVALID_ARGUMENT` with instruction to lower min first). Remote can lower the current minimum to <=70, and then set max <=70. Do not change `PowerLimits::new` itself.
- **ACCEPTED default: `enabled = false` on first launch.** While disabled, both local `egui` and remote Terminal may set channel `Power Limits min/max` anywhere in the normal upstream `0..200` range, subject only to existing upstream normalization and general request validation; there is **no additional remote cap on either A or B**. Previously saved A/B numbers, if any, are inert until Host locally enables the switch.
- With the switch enabled, only the remote `channels.set_limit_min` and `channels.set_limit_max` writes are subject to the **corresponding A or B cap**. Local `egui` is never constrained by this remote policy. Existing core behavior remains unchanged. A/B caps can be edited independently at runtime and policy notifications reflect the new values without reconnecting.
- Same policy for remote REST, WSS and WebRTC DataChannel. Any authenticated transport must pass through the same check before dispatch; never rely only on front-end slider max.
- The policy change and actual `limits.*` changes are published promptly to Terminal; value preservation and disagreement are intentional.

### Explicitly not constrained by Option A

- Other remote operations remain governed only by their existing upstream behavior and ordinary API validation: zone changes, `Intensity` (0–100%), waveform frequency, modulation, tuning, aggregation, presets, config save/load, and Alarm commands.
- Do **not** add policy checks to these operations merely because they may indirectly affect output. Do **not** change the engine's intensity math or current alarm rules.
- Consequently **the actual output can exceed the configured remote cap** when the Host uses a higher local ceiling or an Alarm raises the output. Even remote changes to zones and other properties can increase output with such a Host ceiling. **Never describe Option A as an absolute physical safety limit.**
- No alarm, BLE, OSC, DSP, hardware or device limits are modified in `src-core`.

## Draft transport-neutral API shapes

Read-only policy event (wire shape still proposed):

```json
{"type":"remote.policy","protocol":1,"remote_strength":{"enabled":true,"max_by_channel":{"A":70,"B":120},"scope":"channel_power_limits_min_max"},"session":{"max_remote_sessions":1}}
```

Rejected remote write (wire shape still proposed):

```json
{"type":"command.error","protocol":1,"request_id":"<request UUID>","code":"REMOTE_LIMIT_EXCEEDED","message":"Remote channel A Power Limits cannot exceed 70","channel":"A","allowed_max":70}
```

Proposed transport-neutral state when disabled (wire format remains DRAFT):

```json
{"type":"remote.policy","protocol":1,"remote_strength":{"enabled":false,"max_by_channel":{"A":70,"B":120},"scope":"channel_power_limits_min_max"}}
```

When `enabled=false`, **neither** A nor B cap is enforced regardless of retained `max_by_channel` values. Host may retain separately configured A/B numbers while disabled. On a brand-new Host, values may be `null` until set locally; the master switch cannot be enabled until both A/B numeric values are specified.

## Acceptance matrix

| Scenario | Expected |
|---|---|
| Policy disabled; local A/B, remote A/B Power Limits writes in `0..200` | Accepted according to existing upstream semantics; no remote-cap rejection |
| Policy disabled with stored A=70, B=120; remote `A.max=180`, `B.max=200` | Both accepted; remembered cap numbers are inert |
| Host enables A=70, B=120; remote `A.max=71`, `A.min=71` | Both rejected `REMOTE_LIMIT_EXCEEDED` with `channel=A`, `allowed_max=70`; no enqueue |
| Host enables A=70, B=120; remote `B.max=100` | Allowed if normal upstream validation passes; A cap 70 is irrelevant to channel B |
| Host enables A=70, B=120; remote `B.max=121` | Rejected with `channel=B`, `allowed_max=120`; no enqueue |
| Host changes A cap 70→90, leaving B=120 | A limit changes immediately; B limit remains 120; terminal sees updated policy |
| Host sets actual `A.max=150`, `B.max=180` while caps A=70,B=120 | Remote displays actual 150 and 180 unmodified, alongside each remote cap |
| Host A.min=80, A.max=150, cap A=70; remote `A.max=70` | Reject due to upstream max normalization; lower A.min first |
| Host turns policy OFF; remote writes `A.max=200`, `B.max=200` | Accepted under original upstream constraints |
| Host tries to enable without explicitly configured A and B values | UI refuses, explains missing value; no invented default |
| Remote modifies zones / Intensity / presets / Alarm | No new cap-based block regardless of per-channel limits |
| Remote tries to edit either policy value or master enable | Unsupported/denied on all transports |
| Second remote client or WSS ↔ RTC attempt | One active remote session; no additional slot |
| Network disconnect | Current Host engine and output unchanged |

## Still open (independent of Option A)

- **A/B independent values and default OFF are accepted.** Exact visual layout, copy/control wording and whether separate per-channel enable switches are ever desired remain outside this decision; v0.8 specifies one master switch plus two numeric values.
- Heartbeat 5 s and detection ~15 s accepted (ADR-023); same-session resume and local revoke controls are accepted (ADR-024–027; see `12-session-lifecycle-recovery.md`); exact wire format/cryptographic identity binding remains draft.
- Final wire DTO naming and transport/auth mechanism; no implementation assumed.
