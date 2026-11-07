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
- [ ] Record source identities and confirm the primary checkout and P1 worktree remain untouched.

### Task 2: Merge official upstream history

**Files:** Entire source tree as needed; preserve both parent histories.

- [ ] Merge pinned upstream SHA with an explicit merge commit.
- [ ] Inspect and resolve each actual conflict individually; retain private Host/Terminal modules and official Graph/firmware/protocol features.
- [ ] Record important conflict resolutions in focused commits or the merge message.

### Task 3: Reconcile Graph Mode with Host and remote behavior

**Files:** `src-gui/`, `src-remote/`, `src-terminal/`, `src-app/`, `src-core/` as evidenced by conflicts.

- [ ] Preserve Classic and Graph UI in egui Host and Terminal frontends.
- [ ] Verify runtime/device command ownership, remote snapshots, authorization, mode transitions, disconnect/reconnect, and panic-stop behavior.
- [ ] Reject unsupported remote Graph operations explicitly without bypassing upstream safety or changing Power Limits policy.

### Task 4: Apply P1 Terminal lifecycle fixes

**Files:** `src-terminal/src-tauri/src/commands.rs`, `src-terminal/src-tauri/src/transport/direct_wss.rs`, and associated tests.

- [ ] Apply only commits `546bd8b` and `a05101e` from the P1 line; do not copy its dirty working-tree changes.
- [ ] Adapt to merged APIs and retain deterministic disconnect-while-queue-full and refresh-after-resume regression coverage.

### Task 5: Documentation and validation

**Files:** `README.md`, architecture/build documentation, lockfiles, and integration tests as needed.

- [ ] Document combined functionality, builds, compatibility behavior, and known remote/Graph limitations.
- [ ] Run full Host Rust build/tests, Graph evaluation/editor/render tests, Terminal backend/frontend tests and production build, remote auth/session/reconnect tests, P1 regressions, BLE/protocol fixtures, format/lockfile/diff checks.
- [ ] Verify a fresh clone builds and run dedicated source/history secret scanning, dependency audit, license/provenance, and Actions review.
- [ ] Address confirmed findings or disclose any incomplete scan and residual risk.

### Task 6: Publish and collect evidence

**Files:** Sanitized evidence report and checksum sidecar at the authorized evidence destination.

- [ ] If all publication gates pass, push only `integration/upstream-e3209` to `Zaknin/ShockingVRC-Term`.
- [ ] Verify remote commit/tree SHAs and GitHub CI for that exact commit.
- [ ] Publish sanitized evidence and read back its SHA-256; otherwise report PARTIAL/FAIL with the concrete blocker and preserve the local working branch.
