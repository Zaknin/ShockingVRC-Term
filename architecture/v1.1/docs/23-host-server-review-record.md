> **CURRENT STATUS, 2026-10-15 / v1.1:** OWNER-APPROVED `HOST-P01–HOST-P09`. This was originally a proposal/review document; `PROPOSED` and `REVIEW REQUIRED` below are historical wording. Refer to root `ARCHITECTURE-APPROVAL-v1.1.md` and `APPROVAL-STATUS.md` for authoritative current approval. No code has been implemented by this approval.

# 23. Host Remote Server — proposed decisions and approval record

**Status:** REVIEW REQUIRED · v0.16 · 2026-10-15  
**Normative inherited baseline:** v0.15 API approved, ADR-001–038, ADR-033 conditional.  
**This file is not an acceptance record:** no new Host decisions are approved merely by writing this draft.

## Proposed implementation decisions

| ID | Proposed decision | Reason/status |
|---|---|---|
| HOST-P01 | `src-remote` Rust crate, backend trait; adapter lives in `src-gui` | Keeps remote networking from depending on GUI/internal Engine; PROPOSED |
| HOST-P02 | Adapter maps 37 operations to existing `Bridge` and adds enqueue-result helper | Accurate ACK without changing Worker; PROPOSED |
| HOST-P03 | One shared transport-independent gateway serializes session admission/fencing/policy/enqueue | Same rules for REST/WSS/RTC, no kick/resume race; PROPOSED |
| HOST-P04 | One volatile SessionManager per Host process, existing approved one-slot state machine | No accidental second operator via REST or RTC; PROPOSED |
| HOST-P05 | Explicit DTO projection, coalesced <=10Hz snapshot reader, catalog/log adapters | Avoid engine poll and unbounded WAN queue; PROPOSED |
| HOST-P06 | Host-only Remote Access UI owns enabled state, credentials, cap A/B, pairing, kick | Separates local authority from remote API; PROPOSED |
| HOST-P07 | Single TCP TLS listener (4318 default) + opt-in owned port-mapping lifecycle, later RTC adapter | Direct network works without third-party service; PROPOSED |
| HOST-P08 | Tokio task supervisor with cancellation and bounded shutdown before `shutdown_session` | No GUI blocking or orphaned server/mappings; PROPOSED |
| HOST-P09 | Negative testing, resource caps, contract compliance and fail-closed behavior | Keeps baseline working and new boundary auditable; PROPOSED |

## Items that are NOT new decisions

- One active remote session, 5s heartbeat, 15s lost connection detection, 30s grace, host kick/revoke, ticket rotation, TTL 900s, 120s dedup and 15s resume replay — already accepted.
- Separate A/B remote `Power Limits min/max` request caps, switch OFF by default, local host unrestricted, true values displayed — already accepted. **Not a hardware power limiter.**
- REST companion and WSS frame types, 37 operations, one slot across protocols, max 10Hz network snapshots, 256KiB message limit, default 4318, BYO ICE/STUN/TURN — already accepted.
- Host Ed25519 identity with WSS SPKI/RTC signed DTLS-fingerprint binding — accepted as a **conditional target**, but technical validation not yet complete.
- No change to source `src-core`, Alarm, OSC, BLE, DSP, power algorithms or Worker command queues — already accepted.

## Implementation risks requiring proof (no change in approved behavior)

1. **ADR-033 proof:** actual Rust/Tauri TLS verifier and WebRTC stack must expose required controls; if infeasible, stop and bring a revised trust design for explicit user approval.
2. **Bridge result:** verify `UnboundedSender::send` can be surfaced without changing local error semantics or Worker loop; never fake ACK.
3. **Session fence:** prove no new post-kick enqueue from a stale generation while allowing already-admitted commands to drain.
4. **Remote min/max cap normalization:** Host prechecks only, race with existing Worker state; do not advertise a physical hard limit or atomic CAS.
5. **Snapshot DTO completeness:** non-Serialize status/ZoneEvent/AlarmStatus/DeviceSlotInfo; transport enum conversions, log cursor, prefs adapters and schemas require fixture tests.
6. **Synchronous/async split:** current Bridge constructed in `eframe` app creator, Tokio runtime owned by `main`; remote shutdown must complete before runtime drop without blocking UI.
7. **NAT mapping:** UPnP success is neither WAN reachability nor STUN penetration; safely own/release leases and preserve manual forwarding.
8. **REST-only activity:** `PENDING_ATTACH` and authenticated REST heartbeat must maintain **same single slot**; reject unsupported/unfinished companion endpoints rather than shipping inaccurate placeholder behavior.

## Recommended review decision

Approve HOST-P01–P09 as the **Host Remote Server implementation architecture**, without declaring ADR-033 proven and without authorizing software development yet. Then write the separate Tauri Terminal architecture and, when both are accepted, prepare a milestone-by-milestone Codex task with tests and rollback. Approval must not be interpreted as acceptance of a completed security proof.
