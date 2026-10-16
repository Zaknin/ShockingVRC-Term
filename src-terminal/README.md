# ShockingVRC Terminal

Windows-first Tauri 2 desktop client for an existing ShockingVRC Host. The Terminal contains no Engine, BLE, OSC, Alarm, or DSP implementation. This first M2 slice reads Host state and does not send mutating Remote API commands.

## Development

Prerequisites are the Windows Rust MSVC toolchain, Node.js with npm, WebView2, and the Windows C++ build tools required by Tauri.

```powershell
npm ci
npm run tauri dev
```

The production frontend is built with `npm run build`; `npm run tauri build` creates the Windows package. Neither command launches the Host or connects to a remote service.

## Pair and connect

Create a one-use pairing link in Host Setup and open it on the Terminal, or paste the `shockingvrc://pair/...` link in **Connect**. The Terminal verifies the Host signature and TLS/DTLS identity in Rust before it proves possession of its Ed25519 device key. Pairing links expire after 15 minutes and are consumed by the Host when pairing succeeds.

The Terminal device key and each Host-signed rendezvous grant are stored in Windows Credential Manager; saved Host profiles contain only public connection and identity data. After pairing, choose the saved Host and press **Connect**. Direct WSS and ICE DataChannel both use the same passwordless Remote Session and existing operation protocol. A Host can revoke a paired Terminal in Host Setup.

The app reads snapshots, capabilities, preset catalogs, and log pages through Remote API v1. The Status, Zones, Channels, Tuning, Modulation, Presets, Alarm, Setup, and Logs sections display Host-provided state and retain the existing Host safety limits.
