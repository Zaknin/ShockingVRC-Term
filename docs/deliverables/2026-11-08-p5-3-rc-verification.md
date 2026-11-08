# ShockingVRC P5.3 RC Verification

## Automated source verification

**PASS** on feature branch `codex/p5-remote-graph-parity`, implementation commit `2ded9fdf4c6ca88aa5a79992a0b95abe8c81d18e`.

- `src-terminal`: `npm test` — 193 passed, 0 failed.
- `src-terminal`: `npm run build` — `vue-tsc --noEmit` and production Vite build passed.
- Repository root: `cargo fmt --all -- --check` — passed.
- Repository root: `cargo test --locked --workspace` — passed, including Graph editor, Graph Apply/revision conflict, transport, and lifecycle coverage.
- `src-terminal/src-tauri`: `cargo test --locked --all-targets` — 100 passed, 1 ignored, plus the standalone vendor-resolution integration test passed. The ignored test requires its disposable loopback coturn UDP fixture.
- Repository root: `cargo clippy --locked --workspace --all-targets` — completed successfully with existing warnings in untouched Rust code.
- `src-terminal/src-tauri`: `cargo clippy --locked --all-targets` — completed successfully with existing warnings in untouched Rust code.
- `git diff --check` — passed.

The DOM tests cover catalog-backed menu contents, pointer-coordinate mapping, local draft history, menu boundaries, mode labels, and the obsolete banner. They do not establish native WebView2 mouse behavior.

## Owner acceptance status

**PENDING** for native Windows GUI right-click acceptance. No isolated authenticated Host/Terminal GUI fixture was available in this worktree, so the native browser-menu suppression and WebView2 behavior remain unobserved.

**PENDING** for live WAN Graph Apply, revision synchronization, and concurrent Host edit conflict acceptance. Existing owner-confirmed Internet connection, mode switching, and Graph loading do not establish those Apply outcomes.

No Coyote device test, physical output test, sustained WAN/TURN test, installer run, or existing-profile modification was performed.

## Windows owner checklist

Keep Coyote devices powered off throughout the initial GUI and Graph edit checks. Extract the package to a fresh test folder; do not run the installer for this acceptance pass.

1. Launch `Host\shockingvrc.exe` from the extracted package. Launch `Terminal\shockingvrc-terminal.exe` on the other Windows machine. The portable Terminal must start without Node, npm, or Vite.
2. Connect Terminal to the Host over the Internet. Switch Classic → Graph and Graph → Classic. Check the selector shows the Host's current mode and the confirmation reads `Host confirmed`; verify the obsolete warning is gone.
3. Open the Graph workspace and right-click blank canvas. Confirm the app-owned Add Node menu opens and the WebView2 `Save Page As` menu does not. Search, use arrow keys and Enter, and add a node at the pointer position.
4. Drag a node, pan and zoom the canvas, connect compatible pins, select a Host Graph preset, then Undo and Redo. Confirm edits stay in the local draft until explicit Apply.
5. Confirm Apply, verify the Host Graph revision advances and refreshes in Terminal. For conflict coverage, make a separate Host Graph change after opening the Terminal draft and confirm Apply reports the revision conflict instead of overwriting it.
6. Disconnect and reconnect. Confirm the local draft remains available and stale state is reported when Host Graph changed.
7. Keep physical-device operation and sustained WAN/TURN checks separate from this pass.

## Release artifacts

The final package is built after the verification commit so both Windows binaries embed the same final source commit. Its `MANIFEST.json` records that commit/tree and each artifact SHA-256. Its `README.md` contains the exact local package paths, WebView2 prerequisite, and this owner checklist.
