# Contributing

Thanks for helping improve ShockingVRC. Keep changes focused, preserve the Host as the authority for remote state and commands, and add regression coverage when fixing a defect.

## Before opening a change

- Read [the architecture overview](docs/architecture.md) and the license terms in [LICENSE](LICENSE).
- Keep private endpoints, credentials, invitation links, Host identities, and machine-specific configuration out of source, tests, logs, and screenshots. Use reserved `.test` domains and synthetic values in examples.
- Do not add hardware-output, unattended-service, or production-deployment steps to automated tests.
- For vendored or patched dependencies, record upstream provenance, checksums, license notices, and local changes alongside the code.

## Checks

Run the relevant checks before submitting. From the repository root:

```powershell
cargo fmt --all -- --check
cargo test --workspace --locked
```

For Terminal frontend changes, from `src-terminal`:

```powershell
npm ci
npm test
npm run build
```

For Terminal Rust changes, from the repository root:

```powershell
cargo fmt --manifest-path src-terminal/src-tauri/Cargo.toml -- --check
cargo test --locked --manifest-path src-terminal/src-tauri/Cargo.toml
```

The Windows GitHub Actions workflow runs these checks. Long TURN endurance tests require an isolated local relay fixture and remain separate from routine CI.

## Review notes

Describe the user-visible behavior, the tests run, and any work that still needs owner or maintainer acceptance. Automated tests do not establish desktop GUI acceptance, WAN behavior, TURN endurance, or physical-device safety. Do not claim those results unless they were tested separately with explicit supervision.
