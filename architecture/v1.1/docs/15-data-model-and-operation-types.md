# 15. Remote read-model DTO + command types — approved design v0.15

**Status: ACCEPTED (design only) 2026-10-15, ADR-032.** Types below are wire-visible models **not** Rust auto-serialized `Snapshot`. Normalization is an API boundary only, not engine behavior. Machine-readable design artifact: `schemas/remote-models-v1.schema.json`, `schemas/remote-messages-v1.schema.json`.

## Snapshot DTO

`state` object:

- `bridge_revision`: uint64 (upstream 50-ms tick; no CAS), `active_device`: unsigned index, `devices[]`: `{mac,name,battery|null,connected,reconnecting}`. `active_device_mac`: `string|null` derived for UX (not authoritative conditional execution).
- `vrchat`: `{connected,osc_port,oscquery_port|null}`; `avatar_zones[]`: `{zone_type,id,is_tps,level,velocity,acceleration,recoil}`. JSON finite floats only.
- `config`: `{channel_a,channel_b,ukf,norms}`. Each channel `{zones,frequency[4],intensity[4],limits:{min,max},aggregation,freq_modulation[4],intensity_modulation[4]}`. `frequency` is existing raw 10..255, `intensity` is 0..100%, power min/max 0..200; values in snapshot reflect **actual Host config**.
- `zones[]` entries preserve `{zone_type,name,mode,scale,min_threshold,max_threshold}` even if upstream serde omits default threshold fields. Client needs exact current editor values.
- `status`: `{device_connected,channel_a,channel_b}`; each `{raw_level,strength,frequency[4],active_zones:[{zone_id:{zone_type,name},level}],kinematics:{depth,speed,acc,recoil}}`. Status `strength` can exceed remote edit cap due to Host/Alarm — **do not clip**.
- `alarm`: `{config,status}`. `config`: existing fields `enabled,hour,minute,channels,start_strength,peak_strength,ramp_secs,max_duration_secs,repeats,pulse_on_ms,pulse_off_ms,snooze_mins`; `status`: `phase,strength,elapsed_secs,auto_stop_in_secs,snooze_left_secs,next_fire_in_mins,attempt,retrying,test`.
- `prefs` display `{auto_save,alarm_tab_visible}`: these originate in Host prefs and are **not** present in upstream `Bridge::Snapshot`; implement adapter read without altering engine. If unavailable, do not fake defaults; report capabilities and leave field optional until adapter exists. **v1.0.2 accepted correction:** `schemas/remote-models-v1.schema.json#/$defs/SnapshotState` now explicitly permits strictly typed optional `prefs:{auto_save:boolean,alarm_tab_visible:boolean}` with `additionalProperties:false`. Host advertises feature `snapshot.prefs` in `capabilities.features` iff `prefs` is available; otherwise omit `prefs` entirely and Terminal must not pretend the current Host value is known.
- `remote_policy`: `{remote_strength:{enabled,max_by_channel:{A:null|int,B:null|int},scope:"channel_power_limits_min_max"},max_remote_sessions:1}`. When enabled both values are required integers 0..200; disabled may preserve or omit configured values.

## Catalog DTOs

- `presets`: array in **exact order of Host `Shared.presets.entries`**; each entry `{index,id,name,description,user,preset:{name,description,author,frequency[4],intensity[4],freq_modulation[4],intensity_modulation[4]}}`. `loading`,`error|null`,`source` optional status fields. The raw upstream `PresetEntry` has the stated fields, and does not magically supply stable object-version CAS.
- `zone_presets`: same index-order guarantee; `{index,id,preset:{version,name,description,channel_a:[zone entry],channel_b:[zone entry]}}`; no hidden sortable UUID or atomic index map.
- `logs`: `{cursor,next_cursor|null,lines:[{seq,text}]}`. `seq` is **adapter-assigned** log cursor (not upstream data field). Existing LogBuffer is a rolling VecDeque up to 2000 lines, without stable identifiers. Adapter must implement cursor and reset behavior; if cursor too old, respond `LOG_CURSOR_EXPIRED` and new page.
- `toast` optional UI notification if captured; should not replace `command.accepted` or authoritative `snapshot`.

## v1 normalized enum values (explicit Rust mapping)

| Wire field | Fixed values | Rust source |
|---|---|---|
| `channel` | `A`, `B` | `src-gui/model.rs::Channel` |
| `zone_type` | `Any`, `Pen`, `Orf`, `Touch`, `DGB` | `src-core/osc/types.rs::OldZoneType` |
| `mode` | `Depth`, `Speed`, `Acc`, `Recoil` | `src-core/cli/config.rs::ContactMode` |
| `aggregation` | `Max`, `Sum`, `Average` | `src-core/cli/config.rs::AggregationMode` |
| `alarm.channels` | `A`, `B`, `Both` | `src-core/cli/alarm.rs::AlarmChannels` |
| `alarm.phase` | `Idle`, `Ringing`, `Snoozed` | `src-core/cli/alarm.rs::AlarmPhase` |
| `kind` (modulation target) | `freq`, `intensity` | `src-gui/model.rs::ModKind` |
| `modulation.source` | `Depth`, `Speed`, `Acc`, `Recoil` | `src-core/modulation/config.rs::ModulationSource` |

`ModulationFunction` is a rich Rust enum with numeric variants and recursive `Compose`; to avoid dependence on Serde tagged enum shape the wire uses `{kind:<snake_case>}` and (for `SinPow`,`CosPow`) `{kind:"sin_pow",exponent:number}`, or `{kind:"compose",outer:Function,inner:Function}`. All kinds are explicitly enumerated in JSON Schema; depth <= 8 to prevent hostile recursion (adapter must enforce). This **re-encodes** parameters but does not modify modulation math.

## Commands and exact arguments

`schemas/remote-messages-v1.schema.json` provides one-of typed `command` cases for all **37** upstream-exposed ops from `06-command-matrix.md`. For every operation, unknown mutation arguments are rejected. Nonfinite floats, out-of-bounds segment or string size produce `INVALID_ARGUMENT` *before enqueue*. Core clamps remain as upstream: API shouldn't silently invent new device limits.

`devices.switch` & indexed zone/preset ops retain old index semantics; documented precheck `expected_active_device_mac` is at most best-effort. Unsupported updater commands never present in remote API.

## Observability

- Snapshot `bridge_revision` tracks publication, **not** config version or `command_id` apply proof.
- `seq` server push is monotonic within transport generation, resets on re-auth/reconnect; client re-fetches full state on gap.
- Avoid flooding: 5–10 Hz snapshot delivery target (coalescing), while 50-ms Worker tick remains untouched.
- Snapshot `active_zones` and diagnostics may reveal sensitive runtime information: always authenticated; minimal anonymous `/health`.

## v1.0.2 — HTTP REST response DTOs (F-06)

HTTP JSON responses are **envelope-free** (no WSS `type`/`protocol`). Strict shapes in `schemas/direct-api-v1.openapi.yaml`:

- `GET /capabilities` → `{operations:OperationName[],features:string[]}`; include `snapshot.prefs` only if snapshot publishes `prefs`.
- `GET /catalogs/presets` → `{entries:PresetEntry[]}` in Host order.
- `GET /catalogs/zone-presets` → `{entries:ZonePresetEntry[]}` in Host order.
- `GET /logs` → `{cursor:string,next_cursor:string|null,lines:[{seq:uint,text:string}]}`.
- `POST /commands` → **202** `{request_id:UUID,queued:true}` only after proven enqueue; otherwise **200** `{request_id:UUID}` as mere receipt, **not** an apply ACK.
- `POST /auth/refresh` → `{access_token:string,access_expires_in:900}` for same valid REST session; no slot acquisition.

This is an exact shape clarification; no extra remote capabilities are created.
