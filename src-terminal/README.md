# ShockingVRC Terminal

Terminal is a Windows-first Tauri 2 desktop client for an existing ShockingVRC Host. It reads Host snapshots and sends allowlisted Remote API commands through an authenticated session. It does not include the Host engine, BLE, OSC, or DSP implementation.

## Development

Install Node.js 22, npm, the Rust MSVC toolchain, Windows C++ build tools, and WebView2. From this directory:

```powershell
npm ci
npm test
npm run build
npm run tauri dev
```

To run the Rust backend tests, use the repository root:

```powershell
cargo fmt --manifest-path src-terminal/src-tauri/Cargo.toml -- --check
cargo test --locked --manifest-path src-terminal/src-tauri/Cargo.toml
```

For packaging, build the frontend with `npm run build`, then run `npm run tauri build` from this directory on Windows.

## Pair and connect

Create a one-use pairing invitation in Host Setup and open or paste its `shockingvrc://pair/...` link in Terminal. The Host verifies the Terminal device proof; Terminal verifies the Host identity and certificate before pairing completes. Invitations expire after 15 minutes and are consumed on successful pairing. A Host can revoke a paired Terminal in Host Setup.

Saved Host profiles contain public connection and identity data. The Terminal device key and Host-signed rendezvous grant are stored in Windows Credential Manager. After pairing, choose a saved Host and connect directly or through the configured ICE route. Direct secure WebSocket and WebRTC DataChannel transports use the same Remote API and session model.

Terminal exposes Host-provided status, zones, channels, tuning, modulation, presets, alarm, setup, and logs views. Remote operation limits are enforced by the Host. In particular, the Host's optional remote Power Limits policy only constrains remote writes to channel Power Limits; it is not a general physical-output limiter. See the repository [README](../README.md) and [architecture overview](../docs/architecture.md).
