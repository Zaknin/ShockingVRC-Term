> **v1.1 status override:** the owner completed static audit and re-audit (GO) and approved the full design. References below to proposed/unapproved Host, Term and Infra or pending initial audit are retained for history. Current statuses are in `../APPROVAL-STATUS.md`; owner audit rules remain in force for any future audit.

# 40. Architecture handover — for an OWNER-DEFINED audit later

> **MANDATORY OWNER AUDIT POLICY:** Read root [`OWNER-AUDIT-RULES.md`](../OWNER-AUDIT-RULES.md) **before doing any audit work**. English operating translation: [`AUDITOR-MANDATE-EN.md`](../AUDITOR-MANDATE-EN.md). The owner has explicitly excluded functional tests, deployment, destructive actions, scope creep and generic security hardening from the planned architecture audit. **A user-provided static audit identified F-01–F-06; corrections are included, without a new audit.** Historical test plans are references only and not authorization to execute them. Report format: [`AUDIT-REPORT-TEMPLATE.md`](../AUDIT-REPORT-TEMPLATE.md).

**Package intent:** complete documentation handoff. **No audit performed by the package author in this correction round.** A user-provided static audit was accepted and its six specific findings addressed. This file does not invent additional security, business, production, destructive, or stress-testing requirements.

## Reading order

1. **Mandatory first:** `OWNER-AUDIT-RULES.md`; then root `README.md`, `APPROVAL-STATUS.md`, and `docs/00-scope-baseline.md`.
2. Accepted API norm: `docs/14`–`19`, `schemas/remote-messages-v1.schema.json`, `schemas/remote-models-v1.schema.json`, `schemas/direct-api-v1.openapi.yaml`, `docs/06-command-matrix.md`.
3. Proposed Host: `docs/20`–`23`, detailed Host settings `34`.
4. Proposed Terminal: `docs/24`–`28`, egui-aligned UX `35`.
5. Self-host infrastructure: `docs/29`–`33`, `deployment/` and `schemas/rendezvous-v1.schema.json`.
6. Release/migration/test/roadmap: `docs/36`–`39`, `docs/41`–`45`.
7. Open decisions/gaps: `docs/43-design-status-and-open-items.md`, `docs/44-operational-recovery-and-dependencies.md`.

## Classification legend

- **ACCEPTED DESIGN:** owner explicitly approved requirement/ADR. No implication of implemented code.
- **APPROVED CONSTRAINT:** owner explicitly selected a direction (Windows only, Rust rendezvous+coturn, egui-aligned design, empty endpoints).
- **PROPOSED DESIGN:** architecture presented but not yet separately approved (HOST-P01–09, TERM-P01–12 and detailed infra choices).
- **CONDITIONALLY APPROVED:** ADR-033 target; feasibility dependent on actual Rust TLS/DTLS library prototype.
- **PLANNED TEST:** checklist not executed.
- **M0 EVIDENCE:** original Windows 137/137 tests, NOT full project completion.

## What this package is NOT

- Not a code submission and not a fork commit. Nothing in `src-remote`, `src-terminal`, `src-rendezvous` has been implemented by producing this ZIP.
- Not a deployment instruction to run on Moscow host, router, FortiGate or OpenShock; external installation has not occurred.
- Not a completed security audit, penetration test, threat model review or certified cryptographic proof.
- Not a mandate to modify Alarm, BLE/OSC/DSP, hardware safety or existing power logic.
- Not a public SaaS, cloud availability target, production SLA, or account service.

## Handoff checklist

| Package question | Where to find it |
|---|---|
| What is the exact upstream commit? | `00`, `APPROVAL-STATUS.md` |
| Which behavior is owner-approved? | `05`, `18`, `19`, `43` |
| How many commands/what wire contract? | `06`, `14`, `15`, `schemas/` |
| How does Host avoid duplicating engine? | `20`, `21`, `34` |
| What does Term show/how does it connect? | `24`–`26`, `35` |
| Who hosts public traversal servers? | `07`, `10`, `29`–`31` |
| What happens under CGNAT? | `32`, `38` |
| What is not yet feasible/proven? | `17`, `33`, `43` |
| Which tests run vs planned? | `VALIDATION.md`, `38` |
| What is the implementation sequence? | `39` |
| How are files integrity-checked? | `SHA256SUMS`, `PACKAGE-MANIFEST.json` |

## Review-specific conventions

Reviewers should label findings with exact document path/section and distinguish **contradiction** from **optional improvement**; should not silently overwrite accepted requirements with their own priorities. **No functional testing at all is included in this architecture audit**; even read-only software/network probes require separate owner instruction. Any testing/deployment beyond read-only documentation review awaits the owner's separate instruction. No security controls beyond already accepted design are invented here.

## Post-audit artifact trail (not performed)

When user requests publication: canonical `\\\<evidence-share>\Torrents\Codex\ShockingVRC-Term\...` → independent read-back SHA-256 → sidecar last → optional mirror Google Drive. Neither current ZIP generation nor local hash verification constitutes SMB publication.

## v1.0.2 corrections handover

Review `../AUDIT-CORRECTIONS-v1.0.2.md` first for the precise F-01…F-06 closure map and schema/document locations. The owner accepted all six minimal design corrections; approval of implementation proposals and ADR-033 feasibility remain separate. The six reported issues are addressed **in the documentation**; no re-audit or runtime checks have been performed.
