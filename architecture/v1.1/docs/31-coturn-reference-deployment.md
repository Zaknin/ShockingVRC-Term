# 31. coturn: isolated self-hosted deployment reference

**Status:** DESIGN EXAMPLE, NOT RUN. Use `../deployment/coturn/turnserver.conf.template`, `.env.example` and `../deployment/docker-compose.example.yaml`. These are templates with deliberately missing required values; they must **not** be launched unchanged. No claims about actual Moscow server state.

## Roles

coturn can serve STUN binding requests and allocate TURN relay transports. Its UDP/TCP/TLS availability depends on the real public network and configured endpoints. `TURN` assists ICE; it does not authenticate a ShockingVRC user, open a TCP WSS listener, or rewrite Host commands. Do not treat STUN results as confirmed reachability.

## Reference Linux arrangement

- Rust rendezvous HTTPS/WSS: separate process/container and hostname, operator-selectable port.
- coturn: separate Linux systemd service or Docker service, with explicit `3478/udp` and relay range (e.g. `50000–50100/udp`). Optional `3478/tcp` and `5349/tcp` for restricted networks only after actual configuration/testing.
- If Docker is used on Linux with `network_mode: host`, container directly uses host interfaces; do **not** also publish `ports:`. Use separate persistent config/secret mounts. Verify `host` networking implementation on actual host.
- For public-interface VPS: provide coturn with correct public/external IP if required; for NAT-edge relay, forward listener and relay ports. Limit IP ranges and avoid taking ports already used by OpenShock.
- TURN REST secrets: `use-auth-secret` + a high-entropy `static-auth-secret` injected from deployment secret storage. Do not use a static user password as a substitute when the rendezvous issuer expects HMAC credentials.

## Configuration keys to verify on installed coturn build

| Key | Intent |
|---|---|
| `listening-port` | STUN/TURN listener (`3478` example) |
| `realm` | Operator-controlled origin (`signal.example.net` placeholder) |
| `use-auth-secret` | Short-lived REST-style credential mode |
| `static-auth-secret` | Must match rendezvous issuer; secret is NEVER checked in |
| `min-port` / `max-port` | UDP relay allocation range; adjust for expected allocations |
| `fingerprint` | TURN/STUN message fingerprint setting |
| `no-cli` | Disable unnecessary control listener |
| `external-ip` | If server has private listener behind upstream NAT; format depends on deployment |
| `cert` / `pkey` | Only when TURN-over-TLS is enabled; use real certs |

Check installed version's `turnserver --help` and system service config before applying. Official coturn options: https://github.com/coturn/coturn/wiki/turnserver ; https://github.com/coturn/coturn/blob/master/examples/etc/turnserver.conf .

## Installation plan (does not authorize execution)

1. Read-only inventory of Moscow host: services, OpenShock process/container binding, external interfaces, DNS, firewall and free ports.
2. Select isolated service and listener ports; resolve overlap on paper before any change.
3. Generate unique TURN shared secret **on target host**; never embed it in architectural ZIP, GitHub, app binary or debug output.
4. Install coturn pinning package/container image by digest in implementation; bind secrets through environment/host secret file (avoid command-line secret echo).
5. Enable only chosen inbound UDP relay ports (and TCP/TLS where actually used) without modifying unrelated OpenShock rules.
6. Use `turnutils_uclient`/test RTC to verify relay path; test with separate networks and forced relay, not just local STUN.
7. Observe allocation expiry and packet counters; restart coturn and verify no impact on OpenShock or direct WSS.
8. Document exact actual config, dates, software versions and test evidence when implementation occurs.

## No silent destructive operations

No deployment script in this package should automatically stop containers, replace firewall policy, flush NAT, delete persistent volumes, rotate secrets, or change OpenShock. On failure, return a diagnostic and retain old services; manual operator-directed rollback applies only to new rendezvous/coturn resources.

## Non-goals

- Building a global free TURN service for the public.
- Guaranteeing UDP P2P on every network.
- Configuring OpenShock as a proxy or relay for ShockingVRC.
- Running coturn inside the Windows Host process.
