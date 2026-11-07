# ShockingVRC Upstream Synchronization P3 Implementation Plan

> **For agentic workers:** execute the authorized P3 integration task by task; preserve the published branch and unrelated worktrees.

**Goal:** Produce a buildable integration branch containing the complete published Host/Terminal implementation, relevant official upstream functionality, and both P1 Terminal lifecycle fixes.

**Architecture:** Start from the published `publication/host-terminal` tree, preserve both histories with a real merge of pinned upstream `main`, and reconcile the Host adapter so Graph Mode uses existing runtime ownership and panic safety. Apply only the two P1 commits from the dirty private development line, then validate and publish the new branch only if the specified checks and source/dependency review pass.

**Tech Stack:** Rust workspace, Tauri 2/Vue 3 Terminal, GitHub Actions, npm.

**Spec:** User-provided `pasted-text-1.txt` (Autonomous Upstream Synchronization P3).

## Global Constraints

- Published base: `8f02c830a3744d1b0d94b2236bdc16e5e4a784c9`.
- Upstream main to integrate: `e3209f94fd1a7657e7919289ed9f23957deff639`.
- Common ancestor: `e076bc2ede4726a976c60613031f48c0bc31e0b9`.
- Keep the published branch, dirty primary checkout, and existing P1 worktree unchanged.
- Preserve the private Host/Terminal architecture, Graph Mode, Classic interface, both Host frontends, upstream BLE/protocol behavior, panic safety, and both P1 Terminal lifecycle fixes.
- Never flash firmware or claim physical-device, WAN/TURN endurance, GUI, or electrical-output acceptance from offline checks.
- Push only the new integration branch after required checks pass and no confirmed material exposure remains; do not open PRs, release, deploy, or change the default branch.

## Review Focus

- Graph mode/output ownership across Host commands and remote state: verify explicit rejection for unsupported remote operations and panic-stop remains authoritative.
- Old/new firmware capability fallback and malformed/unsupported protocol packets: verify synthetic transport fixtures and deterministic packet validation.
- Disconnect/reconnect and resumed Terminal session: verify both P1 lifecycle regressions and stale refresh behavior.
- Private secrets/endpoints introduced by merged history or vendored dependencies: run established scanning/audit tools and inspect CI permissions/triggers.
- Fresh-clone reproducibility: build and run the applicable test suites from a clean clone of the final commit.

---

### Task 1: Pin refs and create the integration branch

**Files:** None.

- [x] Verify live publication, upstream, and common-ancestor SHAs.
- [x] Create a fresh managed worktree at the publication commit and branch `integration/upstream-e3209`.
- [x] Record source identities and confirm the primary checkout and P1 worktree remain untouched. The published base is `8f02c830a3744d1b0d94b2236bdc16e5e4a784c9`, upstream is `e3209f94fd1a7657e7919289ed9f23957deff639`, and the primary/P1 worktrees remain dirty and unchanged.

### Task 2: Merge official upstream history

**Files:** Entire source tree as needed; preserve both parent histories.

- [x] Merge pinned upstream SHA with an explicit merge commit (`fe5e96d`).
- [x] Inspect and resolve all 18 actual conflicts individually; retain private Host/Terminal modules and official Graph/firmware/protocol features.
- [x] Record the Host/Graph ownership, safety, release-workflow, and read-only CI resolutions in the merge message and integration documentation.

### Task 3: Reconcile Graph Mode with Host and remote behavior

**Files:** `src-gui/`, `src-remote/`, `src-terminal/`, `src-app/`, `src-core/` as evidenced by conflicts.

- [x] Preserve Classic and Graph UI in egui Host and Terminal frontends.
- [x] Verify runtime/device command ownership, remote snapshots, authorization, mode transitions, disconnect/reconnect, and panic-stop behavior. Add the active engine mode to remote snapshots and disable Classic Terminal edits while Graph mode is active.
- [x] Reject unsupported remote Graph operations explicitly without bypassing upstream safety or changing Power Limits policy. Serialize mode changes with Classic remote configuration admission so a queued Graph switch cannot be bypassed before snapshot publication; keep alarm stop available.

### Task 4: Apply P1 Terminal lifecycle fixes

**Files:** `src-terminal/src-tauri/src/commands.rs`, `src-terminal/src-tauri/src/transport/direct_wss.rs`, and associated tests.

- [x] Verify the two P1 lifecycle changes and regression tests without copying the dirty P1 working tree. Both changes already exist in the published base `8f02c83`, so their cherry-picks were empty and skipped.
- [x] Confirm the published APIs retain deterministic disconnect-while-queue-full and refresh-after-resume regression coverage; both tests pass in the full Terminal backend suite.

### Task 5: Documentation and validation

**Files:** `README.md`, architecture/build documentation, lockfiles, and integration tests as needed.

- [x] Document combined functionality, builds, compatibility behavior, and known remote/Graph limitations.
- [x] Run full Host Rust tests (including Graph evaluation/editor/render, BLE/protocol fixtures, remote auth/session/reconnect), Terminal backend/frontend tests, production frontend build, formatting, and workspace Clippy.
- [x] Verify a fresh clone of commit `893b628` and run the full Host workspace and Terminal frontend/backend suites, production frontend build, source/history secret scan, dependency audits, license/provenance review, and Actions review. The clean clone reproduced the Host and Terminal results.
- [x] Address confirmed findings or disclose any incomplete scan and residual risk. Updated Terminal `plist`/`quick-xml`; the current audits report no known vulnerable dependencies. Remaining advisory warnings and their reachability are recorded in the final evidence report.

### Task 6: Publish and collect evidence

**Files:** Sanitized evidence report and checksum sidecar at the authorized evidence destination.

- [ ] If all publication gates pass, push only `integration/upstream-e3209` to `Zaknin/ShockingVRC-Term`.
- [ ] Verify remote commit/tree SHAs and GitHub CI for that exact commit.
- [ ] Publish sanitized evidence and read back its SHA-256; otherwise report PARTIAL/FAIL with the concrete blocker and preserve the local working branch.
