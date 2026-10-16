# ShockingVRC Rendezvous and TURN operations

This directory contains operator-controlled examples for the standalone Rust `src-rendezvous` and coturn. The samples contain no usable tokens or TURN secret. The checked-in files do not deploy services or change a firewall.

| File | Purpose |
|---|---|
| `rendezvous/config.example.toml` | Rust rendezvous configuration. Defaults bind to loopback and leave public URL/TURN empty. |
| `rendezvous/registration-policy.example.toml` | Per-Host registration key and separate Terminal invite token template. |
| `coturn/turnserver.conf.template` | Linux self-host coturn template with secret placeholder. |
| `coturn/turnserver.local.conf.template` | Loopback-only coturn configuration for a local allocation test. |
| `docker-compose.example.yaml` | Linux service topology template; image references and secrets must be supplied by the operator. |
| `docker-compose.local.example.yaml` | Local coturn container using loopback addresses only. |
| `docker-compose.vps.example.yaml` | Separate rendezvous/coturn Compose project behind an existing Traefik Docker network. |
| `coturn/turnserver.vps.conf.template` | Public IPv4 coturn example with an explicit limited UDP relay range. |
| `.env.example` | Empty endpoint and non-secret port placeholders. |
| `.env.vps.example` | Non-secret VPS image and route variables; replace the rendezvous image tag before use. |

Client/Host distributions have no configured rendezvous, STUN or TURN endpoint. The protocol does not depend on a specific provider. See architecture documents 29–32 and 38–44 for topology and operational boundaries.

## Operator-owned VPS deployment

Set `VPS_PUBLIC_IPV4` and `SIGNAL_HOSTNAME` to values owned by the operator. The example Compose project runs under `/opt/shockingvrc`; keep any existing OpenShock and Traefik projects separate. Traefik must provide TCP 80/443, an `https` entrypoint, an HTTP-01 certificate resolver, and a Docker proxy network.

Rendezvous is reached at `https://<SIGNAL_HOSTNAME>` and WSS signaling at `wss://<SIGNAL_HOSTNAME>/api/signal/v1`. Configure Traefik to terminate public TLS and forward to container port 8087 on the proxy network. The Rust service should not publish a host port.

Coturn may use host networking when the VPS public IPv4 is assigned directly to its external interface; otherwise configure the appropriate external-IP mapping for the deployment. Open only the required TURN control and relay ports in the host firewall. The TURN realm is the signaling hostname. Rendezvous issues short-lived TURN credentials from an operator-held shared secret. Coturn TLS/DTLS may remain disabled when clients use the advertised UDP or TCP TURN transport and public signaling TLS is provided by Traefik.

The VPS has 1 vCPU, 882 MiB RAM, 3 GiB swap, and approximately 7.1 GiB free disk at deployment preflight. Docker Engine 29.5.3 and Compose 5.1.4 are enabled. These small resources are sufficient for the signaling and personal TURN workload; do not run local Rust builds on this VPS.

Deployment smoke so far: public HTTPS `/healthz` and `/version` return successfully; the Windows client validates TLS and completes WSS upgrade; invalid registration and invite credentials are rejected as `auth_required`. Both containers are running and coturn binds IPv4 TCP/UDP 3478. Host registration with the real key, TURN allocation/relay, and the two-network P2P/forced-relay scenarios are still pending owner GUI setup and lab VM access.

### Files and lifecycle

The production files live on the VPS, outside Git:

```text
/opt/shockingvrc/compose.yaml
/opt/shockingvrc/.env
/opt/shockingvrc/rendezvous/config.toml
/opt/shockingvrc/rendezvous/registration-policy.toml
/opt/shockingvrc/rendezvous/turn.secret
/opt/shockingvrc/coturn/turnserver.conf
```

The rendezvous config directory and coturn config are mounted read-only. Keep secret files root-owned and readable only by the relevant service group. Do not copy the completed files into the repository or print them in logs.

From `/opt/shockingvrc`, operate only this project:

```sh
docker compose -p shockingvrc -f compose.yaml ps
docker compose -p shockingvrc -f compose.yaml logs --tail=100 rendezvous coturn
docker compose -p shockingvrc -f compose.yaml up -d
docker compose -p shockingvrc -f compose.yaml stop
docker compose -p shockingvrc -f compose.yaml start
```

For a short ICE investigation, set `SHOCKINGVRC_ICE_DIAGNOSTICS=1` only on the shell command that recreates `rendezvous`; this enables debug records for room/signaling state without enabling debug logging in other containers. Return it to `0` and recreate only `rendezvous` when the investigation ends. coturn verbosity is separate and should be enabled only for a bounded allocation/refresh capture.

Use `docker compose -p shockingvrc -f compose.yaml down` only when intentionally removing these two containers. It does not stop or remove the `openshock` project. Do not run `docker compose down -v`, prune commands, or remove any shared network or volume.

The services use `restart: unless-stopped` and Docker is enabled at boot, so these two containers start again after VPS restart unless an operator stopped them. Verify with the project-scoped `ps` command above and `systemctl is-enabled docker`.

### Identity and authorization

Host ID is derived from the Host Ed25519 public identity and persists in the current Windows user's DPAPI-protected file `%APPDATA%\ShockingVRC\host_identity.dpapi`. Keep the same Windows user profile and file to retain that Host ID. Do not replace the ID in rendezvous policy just because another test profile showed a different ID; that profile has a different Host identity. If the ID unexpectedly changes for the same Windows user, stop and inspect the identity file/profile before editing server policy.

The rendezvous registration policy has one entry per Host ID. Each entry pairs that ID with two different random values: a Host registration key and a Terminal invite token. The Host ID is a stable locator, not a secret or a credential. The Rust service validates both values against this policy; dropping the policy would remove the approved Host registration and per-Host join authorization. For this personal deployment, maintain only the one Host entry that is actually used.

Generate all values on the VPS, not in the repository:

```sh
umask 077
openssl rand -hex 32 > /opt/shockingvrc/rendezvous/host-registration-key.txt
openssl rand -hex 32 > /opt/shockingvrc/rendezvous/terminal-invite-token.txt
openssl rand -hex 32 > /opt/shockingvrc/rendezvous/turn.secret
```

Use the first value only in Host Setup → Remote Access → Registration key. Use the second only in the intended Terminal profile's invite-token field / Windows Credential Manager. The Host identity key, Host API password, registration key, invite token, and TURN secret are separate values. Never send them in chat or place them in Git.

To rotate a registration key or invite token, generate a replacement, update the matching single `[[hosts]]` entry in `registration-policy.toml`, then enter the new registration key in Host and the new invite token in the intended Terminal profile. Restart only the rendezvous container with `docker compose -p shockingvrc -f compose.yaml up -d --force-recreate rendezvous`. For TURN secret rotation, update both the rendezvous `turn.secret` and coturn `static-auth-secret` from the same new random value, then recreate only rendezvous and coturn. Existing allocations expire; reconnect peers after the rotation.

### Host and Terminal setup

On the home Host:

1. Open Setup → Remote Access and enable Remote Access with the owner's chosen Host API credentials.
2. Copy the current Host ID and TLS fingerprint locally. Do not send the fingerprint or password in chat.
3. Enable ICE traversal, set rendezvous URL to `wss://rendezvous.example.com/api/signal/v1`, and enter the VPS-generated registration key. Save it in the Host's protected credential storage.
4. Keep the configured Host ID in the one VPS policy entry. Do not configure a second Host unless the owner intends to authorize it.

On the Windows Terminal:

1. Install `C:\Dev\ShockingVRC-Term\deliverables\ShockingVRC-Terminal-M4b-Test-x64-Setup.exe`.
2. Create a profile for the Host's normal Direct API address/port, current Host ID, and the fingerprint copied from that Host. Keep the pin exact; do not disable TLS verification.
3. Set connection mode to Traversal only for the P2P/relay tests, rendezvous URL to `wss://rendezvous.example.com/api/signal/v1`, and save the invite token in Windows Credential Manager.
4. For the forced relay scenario, enable “Force TURN relay for ICE sessions” in that profile. This sets ICE relay policy for the RTC session; it does not change Direct WSS behavior.

Never put the registration key, invite token, Host password, or Host private identity in a profile export or chat. The installer contains no per-user Host credentials or test profile.

The current installer is Terminal `0.1.0`, a Windows x64 NSIS setup executable, 4,924,678 bytes, SHA-256 `3B20A46B233F2D08EA710C15FDE1EF3F2BB28C0BF3CFAF8690E15F7938BC1976`. The Tauri default WebView2 download-bootstrapper behavior is retained; Windows 10 version 1803 or later and Windows 11 include the WebView2 runtime. If an older supported system lacks WebView2, installation requires internet access to download the bootstrapper.

### Repeatable service checks

From an internet-connected machine:

```powershell
Invoke-RestMethod 'https://<SIGNAL_HOSTNAME>/healthz'
Invoke-RestMethod 'https://<SIGNAL_HOSTNAME>/version'
```

An HTTP `101 Switching Protocols` from an authenticated WSS registration/join confirms signaling transport only, not TURN. Check policy failures with invalid credentials only in an approved isolated test; never place the rejected secret in logs. Run `/healthz` and `/version` after each service change.

For P2P acceptance, set Terminal mode to Traversal only, sign in, read Snapshot, then inspect the connection label for `host` ↔ `srflx`/`prflx` candidate types. A `relay` candidate is not P2P. For forced TURN, enable “Force TURN relay for ICE sessions” and verify the selected pair includes `LOCAL relay` and `REMOTE relay`; then verify read-only Snapshot traffic and heartbeat while connected. A TURN allocation or `ONLINE` alone does not prove relay data transfer. Complete these checks between two independently routed networks. Do not send power, Alarm, or other control commands during read-only acceptance.

Useful public TCP check:

```powershell
Test-NetConnection rendezvous.example.com -Port 443
```

Use a TURN-aware client or the Terminal candidate-pair display to verify an actual allocation and relayed DataChannel traffic. An open 3478 port is not a TURN PASS. When UDP is blocked by a tested network, verify the configured TCP TURN URI separately; do not change that network's firewall as part of this procedure.

### Safe removal

If the owner chooses to remove this deployment, first stop only the `shockingvrc` Compose project, then remove only its exact UFW allowances (if present): TCP/UDP 3478 and UDP 49160:49200. Confirm the rules before deleting them. Remove `/opt/shockingvrc` and the `shockingvrc-rendezvous` image only after confirming both paths/image tags belong to this deployment. Leave `openshock`, Traefik, `openshock_openshock`, all OpenShock files/volumes, and ports 22/80/443 untouched. Do not use global Docker prune commands.

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
