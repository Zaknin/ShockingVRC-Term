# Self-hosted connectivity deployment

This directory contains operator-controlled examples for the standalone Rust `src-rendezvous` and coturn. The samples contain no usable tokens, TURN secret, certificate or advertised public endpoint. Nothing here deploys to Moscow/OpenShock or changes a router/firewall.

| File | Purpose |
|---|---|
| `rendezvous/config.example.toml` | Rust rendezvous configuration. Defaults bind to loopback and leave public URL/TURN empty. |
| `rendezvous/registration-policy.example.toml` | Per-Host registration key and separate Terminal invite token template. |
| `coturn/turnserver.conf.template` | Linux self-host coturn template with secret placeholder. |
| `coturn/turnserver.local.conf.template` | Loopback-only coturn configuration for a local allocation test. |
| `docker-compose.example.yaml` | Linux service topology template; image references and secrets must be supplied by the operator. |
| `docker-compose.local.example.yaml` | Local coturn container using loopback addresses only. |
| `.env.example` | Empty endpoint and non-secret port placeholders. |

Client/Host distributions have no configured rendezvous, STUN or TURN endpoint. A rendezvous instance may be self-hosted on a Linux VPS later; the protocol does not depend on a specific provider. See architecture documents 29–32 and 38–44 for topology and operational boundaries.

## Generate separate local test credentials

Generate three unrelated values on the machine that owns the local test configuration. On Linux/WSL with OpenSSL:

```sh
openssl rand -hex 32  # registration key
openssl rand -hex 32  # invite token; keep separate from the registration key
openssl rand -hex 32  # TURN static auth secret
```

Put only the first two values into a copied `registration-policy.toml`, one entry per Host ID. Put the third value in a separate file referenced by `turn.secret_file`. Do not commit these generated files. The Host registration key, Terminal invite token, ShockingVRC Host password and TURN secret serve different purposes and must not be reused. The invite token is shared out of band with its intended Terminal user.

## Run rendezvous locally

1. Copy `rendezvous/config.example.toml` and `rendezvous/registration-policy.example.toml` to a private local directory outside the repository. Rename the policy copy to `registration-policy.toml` and replace all placeholders with generated test values.
2. For local WSS, create a short-lived self-signed test certificate in that private directory with `openssl req -x509 -newkey rsa:2048 -nodes -keyout server-key.pem -out server.pem -days 7 -subj "/CN=localhost" -addext "subjectAltName=DNS:localhost,IP:127.0.0.1"`, then set both `[server.tls]` paths. Test clients must explicitly trust this local certificate. With both paths blank, the binary serves HTTP and is intended only behind an operator-managed TLS reverse proxy; public use must terminate TLS before clients connect.
3. From the repository root run `cargo run --offline -p shocking_vrc_rendezvous -- --config <private-path>/config.toml`. The default bind is `127.0.0.1:8087`; all configured file paths are relative to the configuration file unless absolute.
4. Check `/healthz` and `/version`, then connect Host/Terminal signaling clients to `/api/signal/v1`. Press Ctrl+C for graceful shutdown. Registration and rooms are memory-only and are cleared when the process exits.

`public_url` is informational only and blank by default. The service does not infer public addresses, discover NAT mappings or contact external services.

The Host needs the room ID and nonce before it can answer the Terminal's offer. Because rendezvous v1 has no separate Host-notification message, a successful `join` sends the same `join.accepted` room assignment to both the requesting Terminal and the registered Host. It does not grant Host API access or create a ShockingVRC Remote Session.

## Local coturn allocation test

The local config binds coturn to `127.0.0.1` and omits public peer ranges. It is not suitable for a VPS. Copy it outside the repository, replace the realm and static secret with the same test secret used by rendezvous, and keep `turn.uris` at `turn:127.0.0.1:3478?transport=udp` for local clients.

`docker-compose.local.example.yaml` uses Linux host networking so coturn's loopback bind and relay addresses match its namespace. It requires a running Linux Docker engine and a locally available coturn image pinned by digest in `COTURN_IMAGE`; `pull_policy: never` prevents an image pull. If Docker is unavailable, use an already installed `turnserver` in WSL with the copied local config. Do not install packages or change services as part of routine validation without the machine owner's approval.

## Future Linux self-hosting

Build the standalone binary for the operator's Linux target. Copy the config, authorization policy, TURN secret and TLS certificate/key to operator-owned locations with suitable OS access controls. Point the TLS proxy at the rendezvous loopback bind, or configure the service's direct TLS paths. Configure coturn separately with the same TURN REST secret and an operator-selected public realm, listener and relay range. Review actual port ownership, public address, NAT mapping and service versions before opening any external listener. These examples are configuration references, not deployment scripts.
