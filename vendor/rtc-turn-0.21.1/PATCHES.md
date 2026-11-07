# Vendored rtc-turn provenance and patch notes

- Package: `rtc-turn` 0.21.1, distributed from crates.io with package checksum `c53078a0696df5a3a5e4947bc550c5e903b59803b252f1d39062d637ebd3d17e`.
- Upstream source: [`webrtc-rs/rtc`](https://github.com/webrtc-rs/rtc), revision `2a732df55ca4bcf26b80914fa0f570cfaf74cfcb`, subdirectory `rtc-turn`.
- The upstream package declares `MIT/Apache-2.0`. Both original license texts are included in this directory. File-level change notices identify the local changes required by the Apache option; this note supplements those notices.
- The package is patched locally for allocation-refresh retry handling, stale-nonce channel binding, allocation-lifetime retry fencing, and bounded diagnostics. The local source changes are in `Cargo.toml`, `src/client/client_test.rs`, `src/client/mod.rs`, `src/client/proto.rs`, `src/client/relay.rs`, `src/client/transaction.rs`, and added `src/client/diagnostics.rs` and `src/client/stale_nonce_test.rs`.
- The root Rust workspace and standalone Terminal workspace both resolve this patched path through their lockfiles. Do not replace it with the registry package without rerunning the TURN retry and resolution tests.
- Registry package members such as the original VCS metadata, lockfile, benchmark, and UDP example are retained as published. `.cargo-ok` is a local Cargo cache marker and is not included.
