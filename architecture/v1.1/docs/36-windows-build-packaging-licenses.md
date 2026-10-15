# 36. Windows build, packaging, versions and licenses

**Status:** PLAN / NO BUILDS. First-stage Host and Terminal are Windows-only; rendezvous/coturn are Linux server services. The repository's original ShockingVRC code carries GPL-3.0; distribution and notices need normal license compliance.

## Expected outputs

| Artifact | Origin | Notes |
|---|---|---|
| `shockingvrc.exe` (Host) | Current egui Rust workspace + new `src-remote` crate | Existing Host binary extended with opt-in Remote Access UI/API |
| `ShockingVRC-Term.exe` | `src-terminal` Tauri 2 + Vue 3 | Remote-only; no DG-LAB/OSC direct control in client |
| `ShockingVRC-Term-setup.exe` | Tauri/NSIS | Proposed **primary** Windows release package; no installer run by docs |
| `.msi` | Optional Tauri MSI/WiX | Add only if tested and useful |
| `svrc-rendezvous` | `src-rendezvous` Linux Rust service | Separate release archive / deployment instructions |
| coturn | Standard upstream package/container | Not bundled as app binary |

Tauri Windows NSIS/MSI docs: https://v2.tauri.app/distribute/windows-installer/ . WebView2 is used as the renderer; check runtime presence at install and behavior when unavailable. Do not conflate compiled `.exe` and NSIS `-setup.exe` output.

## Toolchain and reproducibility

- Pin Rust MSRV/toolchain in repo after validating upstream 1.3.1; keep Cargo.lock for app/binaries.
- Pin Tauri 2, Vue 3, Node LTS and package-lock/pnpm-lock (choose one package manager); do not float dev deps within release builds.
- Build on Windows 11 x64 MSVC toolchain (Visual Studio C++ Build Tools, Rust, Node.js, WebView2), one repeatable documented CLI; no Linux cross-compile requirement.
- Baseline test: `cargo test --workspace` **137/137 passed on M0**. New workspace tests add counts, and must not be presented as 137 once expanded.
- Version strategy: keep upstream version `1.3.1` as pinned baseline reference; new fork's own release tag/semver namespace is separate, e.g. `term-v0.1.0` proposed, not an existing release.
- Produce hashes (SHA256) for release archives, dependency lockfiles, major binaries and evidence zip; record commit SHA. Do not distribute signing private keys in repository or artifacts.

## Packaging decisions

- **Proposed:** NSIS user-mode installer plus a portable ZIP for friends if unproblematic. Keep MSI optional and explicitly unapproved until demonstrated.
- Do not add auto-update, Windows service, startup entry or tray-resident client unless a later ADR requests it. No hidden auto-run.
- Separate application IDs and data paths so Term installation cannot overwrite or corrupt existing Host egui profiles.
- Windows Defender/SmartScreen can warn on unsigned binaries; signing strategy can be decided when distributing, without inventing a business PKI or imposing paid code-signing as a precondition to private testing.
- Support clean uninstall: only Term's own app data through an explicit prompt; do not delete Host settings, upstream presets or exported configs.

## GPL/source notices

The source fork inherits upstream GPL-3.0 obligations. Include license/attribution and source access when distributing modified binaries, even among friends where legally applicable; collect license notices for dependencies as necessary. This is a release housekeeping requirement, not a request to change the application logic. No claim of full third-party license inventory yet; complete at release packaging.

## Build and release acceptance

One controlled Windows machine with exact source SHA builds Host, Term and NSIS; freshly installed Term starts and connects to a test Host; test uninstall leaves current ShockingVRC configs/other apps intact. One-session/auth/parity verified without any hardware change in automated tests; hardware integration only with explicit controlled test plan. Archive checksums independently read back from canonical SMB artifact before publishing sidecar.
