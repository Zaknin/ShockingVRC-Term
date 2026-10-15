# 41. Requirements traceability and ownership matrix

**Status:** design mapping. `PASS` never means feature is implemented. Full test scenario IDs refer to `38-integration-test-matrix.md`.

| Requirement | Status | Owner/component | Contract/document | Planned proof |
|---|---|---|---|---|
| Retain `egui` 1.3.1 working core | ACCEPTED | Existing Host | `00`,`20`,`39` | BASE-01 |
| 37 allowed remote operations; updater local | ACCEPTED | Host API | `06`,`14`,`15` | API-01/02 |
| One Remote Session across transports | ACCEPTED | Host SessionManager | `12`–`14` | SESS-01 |
| 5/15/30 sec liveness/resume | ACCEPTED | Host + Term | `12`,`13`,`14` | SESS-02/03 |
| Token 900s/dedup 120s/replay 15s | ACCEPTED | Host auth | `14`,`18` | SESS-04/AUTH-01 |
| Separate A/B remote Power Limits cap, default OFF | ACCEPTED | Host CommandPolicy | `11`,`14`,`34` | CAP-01..04 |
| Local edits reflected promptly on Term | ACCEPTED | SnapshotPublisher / Term store | `14`,`21`,`25` | SYNC-01/02 |
| Direct WSS / REST companion | ACCEPTED | Host `src-remote` | `14`,`20` | API-03/DIRECT-01 |
| ICE/STUN/TURN and reliable DataChannel | ACCEPTED TARGET | Host/Term transports | `07`,`14`,`32` | ICE-01/02/TURN-01 |
| Host identity binding to TLS/DTLS | CONDITIONAL | Identity adapters | `17`,`33` | AUTH-02/C1..C5 |
| Windows Host/Term v1 | APPROVED CONSTRAINT | Both clients | `24`,`36` | UI-02/DIST-01 |
| Independent Rust rendezvous + coturn | APPROVED CONSTRAINT | Self-host Linux services | `29`,`30`,`31` | SIGNAL-01/TURN-01 |
| No default server STUN/TURN/URLs | ACCEPTED | Host/Term configs | `10`,`29`,`42` | BOOT-01 |
| Tauri near egui dark green styling | APPROVED CONSTRAINT | Vue UI | `25`,`35` | UI-01/02 |
| No forced local Tauri replacement | ACCEPTED SCOPE | Later M6 only | `37`,`39` | N/A |
| Non-destructive private testing | USER CONSTRAINT | Entire project | `00`,`38`,`40` | Project-level controls |
| Moscow VPS co-location is private optional deployment | ACCEPTED | Operator | `10`,`29`,`31` | Not deployed |
| Host Rust crate/boundaries | PROPOSED | Host | `20`–`23` | M1 tests |
| Term architecture/IPC | PROPOSED | Tauri | `24`–`28` | M2 tests |
| Rendezvous internal wire/auth/TTL | PROPOSED | `src-rendezvous` | `30`, rendezvous schema | SIGNAL-01 |
| Installer + optional portable | PROPOSED | Release engineering | `36` | DIST-01 |

## Reconciliation rule

For Remote API v1 use `14`/`15`/approved schemas as normative over legacy `02`, `09`, `11` wording; older docs are historical background. New `29`–`45` cannot amend an accepted wire contract merely by being newer. If reviewers find a real inconsistency, log exact line/field and propose an explicit amendment awaiting owner decision.
