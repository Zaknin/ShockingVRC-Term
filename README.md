# ShockingVRC

ShockingVRC connects DG-Lab Coyote devices to VRChat through OSC. The Host is available as a desktop GUI and terminal UI. The Windows-first Terminal desktop app connects to an existing Host through its authenticated Remote API.

This integration combines the Host/Terminal work maintained in this repository with the current official ShockingVRC application. It retains the Classic interface and adds the upstream Graph programming mode, graph presets, device-color controls, OSC spoof guard, and support for the upstream old and new device firmware protocols.

## Project layout

| Crate or app | Role |
| --- | --- |
| `src-core` | Device, BLE, OSC, protocol, Classic controls, Graph evaluation, and safety logic |
| `src-app` | Shared Host runtime, Graph runtime, state, logging, and configuration |
| `src-gui` | egui desktop Host, including Classic and Graph interfaces |
| `src-cli` | ratatui terminal Host |
| `src-remote` | Optional Host Remote API, sessions, and access settings |
| `src-rendezvous` | Pairing and optional signaling service |
| `src-rtc` | WebRTC transport for traversal connections |
| `src-diagnostics` | Redacted diagnostic collection and export support |
| `src-terminal` | Tauri 2 and Vue Terminal client, maintained as a separate Cargo package |
| `graph-presets/` | Bundled Graph presets |

See [the architecture overview](docs/architecture.md) for component boundaries and transport flow.

## Host modes

The Host has two mutually exclusive programming modes:

- **Classic** configures avatar zones, channels, tuning, modulation, and presets.
- **Graph** connects avatar inputs, processing nodes, and device outputs. The graph runtime owns device commands while Graph mode is active; saved Classic configuration remains available when switching back.

Switch modes in **Setup → Preferences → Mode** or start a session with `--mode classic|graph`. Graph mode includes the upstream panic-stop and safe-output behavior. The egui Host exposes the full node editor and presets; the Terminal surfaces mode and device state but does not offer remote Graph editing or Graph-output commands. Host Remote API writes that are not defined for Graph mode are rejected explicitly.

The upstream BLE/protocol implementation supports the documented old and new device firmware versions. The application does not automatically update or flash device firmware. Never treat software configuration limits as guaranteed physical-output limits.

## Build and test the Host

Install a current stable Rust toolchain. From the repository root:

```powershell
cargo fmt --all -- --check
cargo test --workspace --locked
cargo build --workspace --locked
cargo run --release --locked -p shocking_vrc_gui
cargo run --release --locked -p shocking_vrc_cli
```

The GUI and CLI accept `--port <udp>` for a fixed OSC port, `--scan-timeout <secs>` (default 8), `--skip-update-check`, and `--mode classic|graph`.

## Build and test Terminal

Terminal uses Node.js 22 and npm, plus the Rust MSVC toolchain, Windows C++ build tools, and WebView2 for native development and packaging. From `src-terminal`:

```powershell
npm ci
npm test
npm run build
npm run tauri dev
```

Run the Rust backend checks from the repository root:

```powershell
cargo fmt --manifest-path src-terminal/src-tauri/Cargo.toml -- --check
cargo test --locked --manifest-path src-terminal/src-tauri/Cargo.toml
```

For a Windows release build, first run `npm run build` in `src-terminal`, then from the repository root:

```powershell
cargo build --release --locked --manifest-path src-terminal/src-tauri/Cargo.toml --features custom-protocol
```

## Remote access defaults and limits

Host remote access is opt-in. On Windows, its settings are stored at `%APPDATA%\ShockingVRC\remote_access.json`; the listener and ICE traversal default to disabled, and rendezvous and ICE server addresses default to empty. Pairing uses a one-use invitation and a per-Terminal identity. Optional self-hosted Rendezvous signaling and STUN/TURN services support Internet traversal. Host registration material is protected with Windows DPAPI; Terminal device credentials are stored in Windows Credential Manager.

The optional A/B Power Limits ceiling is disabled by default. When enabled, it constrains configured min/max values for local and remote edits and reconciles existing values. It is not a physical-output limiter and does not govern alarm behavior or every device path. Graph mode does not add remote Graph commands or bypass the upstream panic-stop path.

Automated tests use synthetic transports and protocol fixtures where available. They do not establish physical-device, WAN/TURN endurance, GUI, or electrical-output acceptance.

## CI and release behavior

GitHub Actions checks formatting, the Host workspace on Linux, Windows, and macOS, and the Terminal frontend/backend on Windows. Release publishing is not configured by this integration branch.

## Lineage and license

This project builds on [nullshii/ShockingVRC](https://github.com/nullshii/ShockingVRC) and retains original author attribution. It is distributed under GNU GPL version 3 only (`GPL-3.0-only`); see [LICENSE](LICENSE). Vendored TURN code and local patches are documented in [`vendor/rtc-turn-0.21.1/PATCHES.md`](vendor/rtc-turn-0.21.1/PATCHES.md).
