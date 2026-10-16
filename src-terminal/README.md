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

Create a Host profile with the address, port, and SPKI fingerprint displayed in the Host's local Remote Access setup. The fingerprint must be confirmed out of band before the Terminal will connect. HTTPS and WSS use the same strict SPKI pin; the Terminal validates the Host's signed WSS hello before it sends a username or password.

The password is sent to the Rust login command only when the operator selects **Sign In**. Access tokens and resume tickets stay in Rust memory. The versioned profile file contains only the label, address, port, and public fingerprint. Direct HTTPS/WSS is the only transport in this slice; ICE/TURN is a later milestone.

After login, the app reads snapshots, capabilities, preset catalogs, and log pages through Remote API v1. The Status, Zones, Channels, Tuning, Modulation, Presets, Alarm, Setup, and Logs sections display Host-provided state. All controls that mutate Host state remain outside this first slice.
