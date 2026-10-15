> **CURRENT STATUS, 2026-10-15 / v1.1:** OWNER-APPROVED `TERM-P01–TERM-P12`. This was originally a proposal/review document; `PROPOSED` and `REVIEW REQUIRED` below are historical wording. Refer to root `ARCHITECTURE-APPROVAL-v1.1.md` and `APPROVAL-STATUS.md` for authoritative current approval. No code has been implemented by this approval.

# 28. Tauri Remote Terminal — decisions for owner review

**Status:** REVIEW REQUIRED · 2026-10-15 · v0.17. No new TERM proposal is approved yet. The owner requested completion of the full document set *before* performing an audit and will define the audit method later. This file is an architecture review sheet only.

## Proposed Tauri decisions

| ID | Proposal | Scope/rationale |
|---|---|---|
| TERM-P01 | Separate `src-terminal` Tauri 2 + Vue 3 + TypeScript, Windows-first | Remote app separate from Host; no second Engine |
| TERM-P02 | Native Rust owns WSS/RTC, identity, auth, tokens/ticket; Vue owns UI only | Satisfies v1 protocol and consistent pairing |
| TERM-P03 | Single `ConnectionManager` and `SessionClient` shared by direct and ICE transports | Preserves one session across all routes |
| TERM-P04 | Typed IPC allowlist; high-rate UI state via bounded Channel/coalescing | No unbounded browser/event snapshots |
| TERM-P05 | Multiple saved profiles, at most one connected Host at a time; no prefilled BYO ICE services | Useful UX without multi-client behavior |
| TERM-P06 | Optional Windows Credential Manager/DPAPI saved password; live tokens/tickets memory only | Matches approved credentials policy |
| TERM-P07 | Nine egui sections and all 37 operations, functional parity not pixel-perfect | No hidden omissions or new engine logic |
| TERM-P08 | Authoritative snapshots, no optimistic applied-ACK; reconnect full resync | Local and remote see true Host values |
| TERM-P09 | Minimize keeps connection/heartbeat; close quits and logs out best-effort | Simple v1 lifetime, no hidden tray process |
| TERM-P10 | Direct WSS first (M2), RTC adapter and BYO signaling/STUN/TURN in M4 | Delivery ordering already approved; same wire API |
| TERM-P11 | UI visuals/aesthetic not locked yet, release-only perf measurements after prototype | Avoid premature or false FPS promises |
| TERM-P12 | Test through fixtures and owner-approved non-destructive paired end-to-end runs; audit later with owner-supplied methodology | Project is personal/friends, no public service assumptions |

## Already approved elsewhere — not reopened here

- Remote API v1 REST/WSS/RTC JSON, 37 commands, DTO, 256KiB frames, 30 commands/s, 900s token, 120s dedup, 15s resume replay window, approved error codes.
- One active remote session on Host; 5s heartbeat, ~15s disconnect detection, 30s reservation after detection; Host can kick; secrets rotated/invalidated as designed.
- Remote A/B cap only for writes to `Power Limits.min/max`; default OFF; local Host unrestricted; true values visible; local Alarm/core untouched.
- BYO service URLs and TURN credentials all empty by default; Moscow/OpenShock is private opt-in example, not a distributed default.
- Conditional ADR-033 Host identity binding is a feasibility task, not proven by this proposal. Do not replace it with silent key acceptance.
- Host implementation architecture HOST-P01–P09 from v0.16 is **still pending acceptance**. Moving on to design Terminal is not a vote to adopt it.

## Matters deliberately left for later visual/implementation iteration

1. Precise styling: retain green terminal motifs of current egui, or new design? UI functionality is independent of choice.
2. Native WebRTC Rust crate and SPKI/DTLS pin proof, plus certificate generation/rotation details (ADR-033); must be decided against actual library capability.
3. Packaging preference: NSIS installer vs supplemental portable release; actual size, CPU/GPU, minimized behavior measured after real release build.
4. Whether to add system tray/autostart, language packs or update checker in a later milestone. **Not** required for M2.
5. Whether ordinary expired sessions should prompt manual login or perform opt-in auto-login from saved credentials; v1 Term draft chooses manual explicit login after expiration and strictly forbids automatic login after Host revoke. Owner may prefer a different non-revoke behavior.

## Decision rule for later acceptance

Owner can approve TERM-P01–P12 together or adjust any named item. Only then should the package mark them accepted. **Do not call the entire project “audit-ready” until the owner supplies the audit procedure and confirms the architecture package is complete.** Design approval is not code authorization, runtime conformance evidence or public-hosting approval.
