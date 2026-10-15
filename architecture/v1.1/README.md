# ShockingVRC-Term — Full Architecture Documentation v1.1 FINAL (Owner-Approved)

> **CURRENT STATUS / owner approval 2026-10-15:** API + Host + Tauri Terminal + self-hosted INF/NET/UX/REL/OPS designs APPROVED for owner-directed phased implementation. ADR-033 still conditional. The external static re-audit is GO (F-01…F-06 fixed), V2-01 `$id`/`$ref` addressed. **No Remote application code or deployment is delivered here.** See `ARCHITECTURE-APPROVAL-v1.1.md`, `APPROVAL-STATUS.md`, `CODEX-START-HERE.md`. Old v1.0.2 / `PROPOSED` descriptions below are a historical index, not current approval status.


**Status:** **AUDIT FINDINGS F-01–F-06 CORRECTED IN DESIGN DOCUMENTS; NO AUDIT RERUN / FUNCTIONAL TESTS** · 2026-10-15. An owner-supplied **static architecture audit** identified six design contract gaps; all six corrections are in this v1.0.2 design package. No independent re-audit or program functional tests were performed. No program code, GitHub repository, Moscow OpenShock host, network appliance or physical device changed by this task.

**Exact source baseline:** [`Zaknin/ShockingVRC-Term`](https://github.com/Zaknin/ShockingVRC-Term) branch `feature/remote-access`, commit `e076bc2ede4726a976c60613031f48c0bc31e0b9` (upstream `egui` 1.3.1). M0 Windows unit/render tests 137/137 PASS; **the designed remote system itself is not yet implemented**.

## Mandatory future audit instructions (owner-specific)

**Start here before any audit:** [`OWNER-AUDIT-RULES.md`](OWNER-AUDIT-RULES.md). Optional English operational summary [`AUDITOR-MANDATE-EN.md`](AUDITOR-MANDATE-EN.md); report template [`AUDIT-REPORT-TEMPLATE.md`](AUDIT-REPORT-TEMPLATE.md). The owner expressly prohibits running functional tests during the architecture audit, unrequested actions and destructive methods, and rejects generic enterprise/security hardening or speculative scope creep. **This package update does not start the audit.**

## Inherited approved core

The extracted v0.17 material is preserved: original `docs/00`–`28`, existing `schemas/remote-*`, OpenAPI, examples, v0.17 historical validation. Remote API v1 **accepted** by owner (`14`–`19`, ADR-028–038; ADR-033 conditionally). Host architecture `HOST-P01`–`HOST-P09` and Terminal `TERM-P01`–`TERM-P12` remain **PROPOSED** until separately approved. Nothing newly added rewrites their approved API contract.

## Owner constraints confirmed for v1.0

1. Initial Host and Terminal: **Windows only**. Rendezvous and coturn: Linux self-hosted backend services (does not contradict Windows client scope).
2. **Own standalone Rust rendezvous** inside project plus standard coturn. Each user self-hosts; **no built-in default endpoints** or shared free public infrastructure.
3. **Tauri 2 / Vue 3 look stylistically close to existing egui** (dark/terminal green), all existing function parity. Local egui retained in first phase.
4. One active Remote Session and approved 5/15/30s recovery, password/ticket semantics; no device semantics changes.
5. Host-only independent A/B Remote Maximum Strength cap, OFF default, affecting only remote Power Limits min/max writes, not displayed values or actual engine limiter.
6. Project is intended for private/friends use. No destructive tests or unapproved changes to Alarm, OSC, BLE, DSP, power logic or queues. User will prescribe the later **audit** methodology.

## Main sections

| Read | Contents |
|---|---|
| [`APPROVAL-STATUS.md`](APPROVAL-STATUS.md) | Exactly what's accepted/conditional/proposed |
| [`docs/40-audit-handover-index.md`](docs/40-audit-handover-index.md) | Read order and future audit handover |
| `docs/00–19` | Baseline, architecture and **approved Remote API v1** |
| `docs/20–23` | Proposed Host Remote Server architecture |
| `docs/24–28` | Proposed Tauri Terminal architecture |
| [`docs/29-self-hosted-topology-and-deployment.md`](docs/29-self-hosted-topology-and-deployment.md) | Linux self-host network topology + Moscow co-location boundary |
| [`docs/30-rendezvous-service-contract.md`](docs/30-rendezvous-service-contract.md) | Rust rendezvous schema, roles and signaling flow |
| [`docs/31-coturn-reference-deployment.md`](docs/31-coturn-reference-deployment.md) | coturn TURN-REST relay and deployment expectations |
| [`docs/32-connectivity-policy-and-diagnostics.md`](docs/32-connectivity-policy-and-diagnostics.md) | Direct/DDNS/ICE/relay fallback |
| [`docs/33-host-identity-feasibility-plan.md`](docs/33-host-identity-feasibility-plan.md) | ADR-033 implementation proof gates (not executed) |
| [`docs/34-host-egui-remote-settings-ui.md`](docs/34-host-egui-remote-settings-ui.md) | Host Remote Access UI |
| [`docs/35-tauri-ui-style-parity-and-flow.md`](docs/35-tauri-ui-style-parity-and-flow.md) | Tauri egui-aligned UI screens and flows |
| [`docs/36-windows-build-packaging-licenses.md`](docs/36-windows-build-packaging-licenses.md) | Windows EXE/NSIS/release/licenses |
| [`docs/37-later-local-tauri-migration.md`](docs/37-later-local-tauri-migration.md) | Optional future local egui→Tauri |
| [`docs/38-integration-test-matrix.md`](docs/38-integration-test-matrix.md) | Detailed 30+ scenario test matrix, planned only |
| [`docs/39-implementation-roadmap.md`](docs/39-implementation-roadmap.md) | M0–M6 sequencing and implementation boundaries |
| [`docs/41-requirements-traceability.md`](docs/41-requirements-traceability.md) | Requirement-to-source-to-test matrix |
| [`docs/42-configuration-contract.md`](docs/42-configuration-contract.md) | Config hierarchy, defaults, secret placement |
| [`docs/43-design-status-and-open-items.md`](docs/43-design-status-and-open-items.md) | Proposals and remaining technical unknowns |
| [`docs/44-operational-recovery-and-dependencies.md`](docs/44-operational-recovery-and-dependencies.md) | Operations and non-destructive rollback |
| [`docs/45-technical-reference-links.md`](docs/45-technical-reference-links.md) | Technical references |
| [`docs/46-owner-and-friend-setup-guide.md`](docs/46-owner-and-friend-setup-guide.md) | Host/Terminal pairing, DDNS, traversal user journeys |
| [`docs/47-codex-implementation-task-briefs.md`](docs/47-codex-implementation-task-briefs.md) | Scoped post-audit milestone coding briefs |
| [`deployment/`](deployment/README.md) | Incomplete self-hosted coturn/rendezvous templates |
| [`schemas/`](schemas/remote-messages-v1.schema.json) | Remote API and proposed signaling JSON schemas |
| [`VALIDATION-v1.0.md`](VALIDATION-v1.0.md) | Local documentation-only checks |
| `PACKAGE-MANIFEST.json` and `SHA256SUMS` | Per-file integrity checks |

`docs/README.md` is intentionally preserved as the historical **v0.17** index. This root README is the current **v1.0** index. Older draft references can contain obsolete proposal wording: `docs/14`–`19` are normative for Remote API, status/traceability for v1.0 is in `APPROVAL-STATUS.md`/`docs/43`.

## Owner-supplied static audit (F-01–F-06 resolved in design; no repeat audit)

The owner supplied an architecture audit and approved minimal corrections F-01–F-06. See `AUDIT-CORRECTIONS-v1.0.2.md`. Further review is owner-directed; this package is not a finding that implementation is validated. Reviewer should not treat static JSON validation as successful TLS/ICE/real hardware testing. No generic public service scope or destructive methods should be inferred.
