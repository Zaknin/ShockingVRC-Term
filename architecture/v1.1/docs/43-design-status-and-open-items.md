# 43. v1.1 final owner decision / status register

**Date:** 2026-10-15. Owner now approves the full detailed architecture; **approval does not imply implementation**. Historical initial proposal table entries below are retained for traceability; current status is APPROVED except conditional ADR-033.

## Explicitly owner-approved

- Initial **Windows-only** Host/Terminal scope; Linux self-hosted rendezvous/coturn infrastructure.
- Dedicated **Rust** self-hosted rendezvous service included in our project; coturn deployment by each user; **no bundled default URLs/servers/credentials**.
- Tauri Terminal visually **similar to current egui**, preserving nine original sections and remote operation parity.
- All previously accepted ADR-001–038 behavior as recorded in `05`, `18`, `19`; ADR-033 remains *conditional*.
- Personal/friends-only testing, no forced corporate SaaS architecture, no destructive operations, no changes to core hardware algorithms for a remote-control feature; final audit procedure comes from owner later.

## Detailed module design proposals now **explicitly APPROVED by owner** in v1.1

| ID | Proposal | Reference |
|---|---|---|
| INF-P01 | `src-rendezvous` Rust Tokio/Axum service; in-memory room/registry for v1 | `30` |
| INF-P02 | Separate Host registration authorization and ephemeral room invite; not Host password | `30` |
| INF-P03 | TURN REST issuer in rendezvous, HMAC timestamp ephemeral creds | `30`,`31` |
| INF-P04 | Rendezvous room TTL 60s, candidate cap 64, signal frame cap 64 KiB | `30` |
| INF-P05 | Optional coturn 3478/5349 and test relay range 50000–50100 | `31` |
| NET-P01 | `auto` direct-first then ICE/TURN; explicit transport override | `32` |
| UX-P01 | Host Remote Access setup grouping and read-only policy | `34` |
| UX-P02 | EgUi-aligned charcoal/green Vue UI component spec | `35` (style constraint APPROVED, details PROPOSED) |
| REL-P01 | NSIS primary installer, optional portable ZIP, release tag namespace | `36` |
| OPS-P01 | Deployment and recovery steps restricted to newly owned services | `44` |

**Host HOST-P01–P09** in `23` and **Terminal TERM-P01–P12** in `28` were separately presented for review and are now **APPROVED together with the full architecture by the owner on 2026-10-15**. The same is true of INF/NET/UX/REL/OPS proposal IDs above. Their original `P` identifiers are retained unchanged for cross-references.

## Open technical feasibility questions (audit may evaluate later)

1. **ADR-033**: actual library ability and robust Host identity TLS/SPKI + DTLS binding, including cross-transport re-pair/no silent acceptance; must prototype before credentialed implementation.
2. Exact chosen Rust WebRTC crate/release and API stability; candidate currently webrtc-rs but not pinned.
3. Coturn operator's real public interfaces, actual Moscow server port inventory, OS, firewall, coexistence with OpenShock; **unknown until read-only survey**.
4. Tauri real Windows CPU/GPU/minimize behavior and egui FPS impact; **not measured**.
5. GUI catalog/index revision races, DTO serialization and session ACK edge behavior; API target accepted, code proof pending.
6. Selection of final actual production package/installer mode after Windows builds and tests.

These are known engineering proof points, not requests for more user questionnaires or excuses to revise existing power/Alarm behavior. Owner has already supplied `OWNER-AUDIT-RULES.md` and a static audit report; any new audit requires a separate owner request.

## v1.1 final approval and post-audit result

- `ARCHITECTURE-APPROVAL-v1.1.md` supersedes any review-era **approval status** in historical docs, not technical design content.
- User-supplied static re-audit: **GO**, F-01…F-06 closed in documentation; V2-01 optional fixed by schema `$id` alignment.
- ADR-033 remains **CONDITIONAL**, with library feasibility proof pending. Nothing newly implemented or deployed.
