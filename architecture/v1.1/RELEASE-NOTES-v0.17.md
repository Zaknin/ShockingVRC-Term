# ShockingVRC-Term — Architecture v0.17 / Terminal Review

**Date:** 2026-10-15  
**Scope:** Complete Tauri Remote Terminal architecture phase.  
**Approval state:** API v1 approved in v0.15; Host internal architecture (HOST-P01–P09) proposed in v0.16; Terminal architecture (TERM-P01–P12) proposed here; ADR-033 conditional as before.

## What's new

- `docs/24-tauri-terminal-architecture.md`: independent Windows Tauri 2 + Vue 3 Term, Rust network client, session state machine, direct WSS + WebRTC DataChannel, no second engine.
- `docs/25-tauri-terminal-ui-and-state.md`: all 9 GUI sections and 37 remote commands, Host snapshot truth, read-only policy, reconnect/disconnect behavior.
- `docs/26-tauri-transport-ipc-and-storage.md`: Tauri typed IPC boundary, profiles, credentials, WSS/RTC identity verification, token/ticket placement.
- `docs/27-tauri-terminal-validation-and-delivery.md`: focused, non-destructive functional test plan and Windows build/packaging expectations.
- `docs/28-tauri-terminal-review-record.md`: 12 new **proposed** decisions, plus consciously open choices.
- Updated `docs/README.md`, this release note, validation and checksum manifest.

## Explicitly unchanged

All earlier normative documents and machine-readable `schemas/*.json`, `schemas/*.yaml` and examples are **byte-for-byte retained** from v0.16. No API v1 wire changes and no Host assumptions silently approved. No code, GitHub, SMB, DNS, TURN, OpenShock, router or other infrastructure changes.

## Review sequence

1. Owner reviews Terminal architecture TERM-P01–P12, can approve or amend.
2. Complete any remaining documentation topic (including Tauri local migration or self-hosting runbooks, if requested).
3. Owner supplies audit objectives/methodology/limits.
4. Only then package and submit for owner-directed audit, not a generic compliance audit.
5. Codex implementation instructions come only after architectural and audit stages as directed by owner.
