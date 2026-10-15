# 19. API v1 approval record — 2026-10-15

## Decision

The user approved the **entire Remote API v1 design package presented in v0.14** on 2026-10-15 (Russian: «утверждаем»). New proposals P-028–P-038 are adopted as **ADR-028–ADR-038**, as recorded in `05-decision-log.md` and `18-proposal-decisions-v1.md`.

Approval includes the full HTTPS REST companion as well as WSS and WebRTC/ICE transport-neutral commands, 37-operation allowlist, typed DTO/errors, token lifetime and replay/dedup bounds, network snapshot cadence/rate/size defaults, and one-slot REST attach policy. All pre-existing ADR-001–027 remain in force. Existing 137/137 upstream Windows tests are baseline evidence only.

## Conditional technical gate: ADR-033

Identity design uses a pinned long-term Ed25519 Host identity and TLS-SPKI / DTLS-fingerprint binding, but this is a *conditionally accepted target*. Before implementing remote access carrying credentials or control traffic, prove the selected Rust/Tauri/WebRTC stack can enforce it. Document canonical signed transcript and certificate/SPKI validation, simulate MITM signaling/TURN and certificate changes, and obtain explicit approval of any necessary material design change. **No disabling TLS checks or replacing verified pairing with silent trust-on-first-use.**

## No change to baseline or operational behavior

- Rust core/OSC/BLE/Alarm/Worker timing, physical output logic and queue semantics unchanged.
- One remote slot; localhost egui works independently. Shared active device remains upstream semantics.
- Host-defined A/B remote cap checks only remote `Power Limits min/max` writes; master OFF by default, no actual-output limiting.
- Session heartbeat 5s, disconnect detection ~15s, reconnect reservation 30s after detection.
- No preconfigured rendezvous, STUN or TURN service; end user is free to self-host. Moscow/OpenShock deployment is private example only.
- This approval creates **no software implementation** and **no evidence publication**. Schemas and OpenAPI are accepted design documents, not compiled services.

## Next architecture work

1. Define `Host Remote Server` module and GUI integration, dependency boundaries and listener/auth/transport lifecycle.
2. Define interface between `Bridge` and transport-neutral command dispatcher; map snapshots/catalogs/logs and avoid claiming `Worker` apply confirmation from enqueue.
3. Prototype identity and certificate verification as an explicit technical gate.
4. Then design Tauri native Rust networking + Vue views and self-hosted signaling/coturn integration with contract conformance tests.

**Repository:** `Zaknin/ShockingVRC-Term`, branch `feature/remote-access` at baseline `e076bc2ede4726a976c60613031f48c0bc31e0b9` before code changes.

## Accepted correction record — v1.0.2 (2026-10-15)

The owner accepted **all six narrowly scoped corrections F-01–F-06** in the supplied architecture audit. These are amendments to the approved v1 design, not new scope: REST bearer refresh, optional typed prefs Snapshot, `auth.result.session_state`, per-type rendezvous message requirements, self-host rendezvous credential input/storage path, and concrete REST success response schemas. See root `AUDIT-CORRECTIONS-v1.0.2.md` and OpenAPI/JSON Schema. ADR-033 remains conditional; HOST/TERM/INF proposals keep their previous status.
