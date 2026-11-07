# ShockingVRC

ShockingVRC connects a DG-Lab Coyote to VRChat through OSC. The Host is available as a desktop GUI and terminal UI. A separate Windows-first Terminal desktop app can connect to an existing Host through its authenticated Remote API.

## Project layout

| Crate or app | Role |
| --- | --- |
| `src-core` | Device, BLE, OSC, and control logic |
| `src-app` | Shared Host runtime, state, logging, and configuration |
| `src-gui` | egui desktop Host |
| `src-cli` | ratatui terminal Host |
| `src-remote` | Optional Host remote API, sessions, and access settings |
| `src-rendezvous` | Pairing and optional signaling service implementation |
| `src-rtc` | WebRTC transport used for traversal connections |
| `src-diagnostics` | Redacted diagnostics and export support |
| `src-terminal` | Tauri 2 and Vue Terminal client; maintained as a separate Cargo package |

See [the architecture overview](docs/architecture.md) for component boundaries and connection flow.

## Build and test the Host

Install a current stable Rust toolchain. From the repository root:

```powershell
cargo fmt --all -- --check
cargo test --workspace --locked
cargo build --workspace --locked
cargo run --release --locked -p shocking_vrc_gui
cargo run --release --locked -p shocking_vrc_cli
```

The GUI and CLI accept `--port <udp>` for a fixed OSC port, `--scan-timeout <secs>` (default 8), and `--skip-update-check`.

## Build and test Terminal

Terminal uses Node.js 22 and npm, plus the Rust MSVC toolchain, Windows C++ build tools, and WebView2 for native development and packaging. From `src-terminal`:

```powershell
npm ci
npm test
npm run build
npm run tauri dev
```

Run its Rust backend tests from the repository root:

```powershell
cargo fmt --manifest-path src-terminal/src-tauri/Cargo.toml -- --check
cargo test --locked --manifest-path src-terminal/src-tauri/Cargo.toml
```

For a Windows release build, first run `npm run build` in `src-terminal`, then from the repository root:

```powershell
cargo build --release --locked --manifest-path src-terminal/src-tauri/Cargo.toml --features custom-protocol
```

## Remote access defaults and limits

Host remote access is opt-in. On Windows, its settings are stored at `%APPDATA%\ShockingVRC\remote_access.json`; the listener and ICE traversal default to disabled, and rendezvous and ICE server addresses default to empty. The default invitation mode is a route preference and does not start a listener or contact a service by itself. Host Setup contains the remote access controls.

Pairing uses a one-use invitation and a per-Terminal identity. Optional self-hosted Rendezvous signaling and STUN/TURN services support Internet traversal. The Host's rendezvous registration key and identity are protected with Windows DPAPI; Terminal device credentials are stored separately in Windows Credential Manager.

The optional remote Power Limits policy restricts remote writes to the Host's channel Power Limits. It is not a physical-output limiter and does not cover every output path, including alarm behavior, presets, zones, and intensity controls. Do not treat remote settings as a substitute for device safeguards or supervised operation. This repository's automated tests do not establish GUI, Internet/WAN, TURN endurance, or physical-device acceptance.

## Lineage and license

This project builds on [nullshii/ShockingVRC](https://github.com/nullshii/ShockingVRC). It is distributed under the GNU General Public License v3; see [LICENSE](LICENSE). Vendored TURN code and local patches are documented in [`vendor/rtc-turn-0.21.1/PATCHES.md`](vendor/rtc-turn-0.21.1/PATCHES.md).

Contributors should read [CONTRIBUTING.md](CONTRIBUTING.md) before proposing changes.
