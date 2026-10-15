# v1.1 static artifact validation — 2026-10-15

**Only documentation/artifact validation; no application compile, runtime, network, device interaction, deployment or new architecture audit.**

- Source: `ShockingVRC-Term-Full-Architecture-v1.0.2-AUDIT-CORRECTIONS-20261015.zip` (94-file v1.0.2 design archive).
- Current design approval recorded in `ARCHITECTURE-APPROVAL-v1.1.md` and `APPROVAL-STATUS.md`.
- `schemas/remote-models-v1.schema.json` `$id` aligned with existing relative message schema `$ref` (V2-01).
- All JSON Schema files: valid Draft 2020-12 syntax; OpenAPI YAML parsed and version checked.
- Existing top-level remote-message fixtures accepted with registry keyed **only by declared model `$id`**: `auth.login.json, command.set_limit_max.json, command.accepted.json, auth.resume.json, remote.policy.json, hello.json, command.modulation.clear.json, auth.result.json`.
- Unmodified existing remote messages, rendezvous message definitions, OpenAPI v1, existing example fixtures and owner audit policy checked against v1.0.2 bytes.
- Every file listed in `PACKAGE-MANIFEST.json` and `SHA256SUMS` checked against file size and SHA-256; ZIP extraction and CRC checked after packaging.
- Markdown link references checked for missing in-package local targets; unresolved historic links: 0 (see packaging stdout if any).

**Baseline 137/137 Windows tests referenced in prior docs are historical only; no tests run in this v1.1 assembly.**
