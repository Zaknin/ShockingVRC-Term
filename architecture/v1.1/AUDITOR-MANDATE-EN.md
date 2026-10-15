# ShockingVRC-Term — AUDITOR MANDATE (owner instructions)

**DO NOT START THE AUDIT UNTIL THE OWNER EXPLICITLY REQUESTS IT.** This file sets the mandatory method for a future review of the v1.0 architecture package. It does not authorize execution or edits.

## Context and priorities

- Private, friends-only Windows-first desktop/remote-control project; not a public SaaS, enterprise rollout or commercial offering.
- Review **feasibility and internal consistency of owner-approved functionality**, not compliance with global security maturity standards.
- The user's decisions take priority over reviewer preferences. Do not introduce new features or expand requirements.

## Allowed work only after explicit authorization

Read-only documentation analysis: cross-check Host/Terminal/remote API/rendezvous/coturn/WSS/ICE/TURN contracts, identify concrete functional contradictions and implementation blockers, cite the exact file/section and propose the smallest workable correction.

## Prohibited by default

- No changes to repository, documents, services, routers, physical devices, configurations, credentials, or deployment. No autonomous commits/PRs.
- No destructive operations under any circumstances unless the owner separately and explicitly changes that constraint.
- **No functional testing during this audit**: no `cargo test`, smoke tests, BLE/OSC/hardware tests, UI automation, network probes, stress/fuzzing, penetration testing, or new test-coverage gates. Test scope, place, timing and sufficiency are determined by the owner only.
- No redundant or repeated testing requirements, security/perfection gatekeeping, formal certifications, threat-model expansion, enterprise authentication, HA, cloud platform redesign, extra roles, extra guardrails, large refactors, or defensive changes without an owner request.
- No upstream engine redesign for hypothetical regressions. Existing Alarm, BLE, OSC, DSP, modulation, command queue, and power behavior remain as agreed.
- Do **not** silently remove already approved authentication, certificate pinning, session policy, or per-channel remote power limits; these are requirements, not reviewer-added controls. Concrete defects leading to unauthorized remote control or contradicting those requirements may be reported with evidence.

## Findings format

For each finding give (1) `BLOCKER`, `REAL_GAP` or `OPTIONAL`; (2) exact document path/section; (3) demonstrated failing user scenario and supporting evidence; (4) minimum correction; (5) whether the issue is confirmed, a library-feasibility unknown, or speculation. Do not promote assumptions or optional improvements into blockers. If there are no material findings, report that plainly. Do not patch anything.

Read the Russian `OWNER-AUDIT-RULES.md` as normative; this English file is an operational translation. If there's a discrepancy, Russian owner's instructions prevail. Later explicit owner instructions supersede these rules.
