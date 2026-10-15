# Self-hosted connectivity templates — NOT DEPLOYED

These are incomplete operator templates for the future standalone Rust `src-rendezvous` and coturn. They contain only placeholders, no real credentials. **Do not run without reviewing installed package versions, port ownership, external IP and already-running services.** Nothing here has been deployed to Moscow/OpenShock.

| File | Purpose |
|---|---|
| `docker-compose.example.yaml` | Linux coturn service + proposed separate rendezvous image placeholders, non-runnable until built/configured |
| `rendezvous/config.example.toml` | New Rust signaling service's proposed configuration keys |
| `coturn/turnserver.conf.template` | coturn options and secret placeholder |
| `.env.example` | Non-secret ports/hostnames and missing secret references |

Reference instance: a self-hosted server reachable over public internet. Client/Host distributions **contain no server addresses**. Users may choose another VPS and fully separate TURN and signaling machines. Validate with `docs/29-31`, `38`, `44` before running in any environment.

Suggested commissioning path: **read-only inventory → configure own values → validate test instance → start only new units → verify old service unaffected → explicit owner acceptance**. This is not an automation script.

## v1.0.2 — manual rendezvous key provisioning

The standalone rendezvous operator generates **per-Host registration keys** and **per-Host invite tokens** (F-05) and configures them in their own instance's registration/authorization policy file. Give the registration key to the Host owner (egui Remote Access → Traversal); give the invite token to the intended Terminal user (Connect → ICE/Rendezvous). Both are entered manually, start blank, and are distinct from the Host API password. No provider-supplied defaults and no production issuance script are included in this documentation-only package.
