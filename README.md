# Shocking VRC

Bridge gap with DG-Lab Coyote and VRChat via OSC.

## Desktop GUI (egui):
```sh
cargo run --release -p shocking_vrc_gui
```
## Terminal UI (ratatui):
```sh
cargo run --release -p shocking_vrc_cli
```

Both accept the same flags: `--port <udp>` (legacy fixed OSC port; the default
is automatic via OSCQuery), `--scan-timeout <secs>` (default 8), and
`--skip-update-check`.