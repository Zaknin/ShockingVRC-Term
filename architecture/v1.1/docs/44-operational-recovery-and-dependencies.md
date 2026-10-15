# 44. Operational guide, dependency failures and rollback boundaries

**Status:** planned operational reference, no services installed. No destructive methods and no changes to existing OpenShock by default.

## Failure matrix

| Problem | What should continue | Operator action later |
|---|---|---|
| Direct listener fails to bind | Local egui/Engine, BLE/OSC continue | Check chosen port and status; choose another port in Host GUI |
| UPnP mapping fails | LAN/VPN direct listener available | Manual forwarding, check CGNAT; no router factory reset |
| DDNS name stale | Existing session may continue if connected; new Direct may fail | Update external DDNS using router/provider; retest address |
| Rendezvous offline | Direct WSS still available; established RTC potentially continues | Inspect service logs; restart **only new rendezvous** after consent |
| TURN allocation fails | Direct or ICE P2P if reachable | Validate ephemeral creds and UDP relay ports, do not touch OpenShock |
| Host restarts | Local engine startup per upstream; old remote sessions invalid | Login again after trust verification |
| Term crashes | Host engine unaffected; remote slot eventually expires | Relaunch Term; normal login or resume if process token survived is not presumed |
| Password reset / Host kick | Local output and Alarm state remain unchanged | Reauthorize manually, no silent reconnection |
| Identity key rotated/lost | Old pin no longer trusted | Re-pair explicitly through Host local screen |
| API/Term incompatible version | Current local Host remains usable | Match supported protocol version, no hidden auto-update |

## Rollback discipline after implementation

Feature-branch Git commits are isolated. Do not rewrite upstream `egui` baseline. Rolling back Remote Access means disabling listener/registration/owned mappings or running the preserved known-good Host binary; it does not restore a speculative engine state or flush a network appliance. Deployment rollback applies only to newly owned coturn/rendezvous units and their newly created firewall rules, with exact before/after logs.

## Co-location checklist for Moscow server (read-only first)

- Verify target actual identity and installed OpenShock routes before choosing service ports.
- Map current bound TCP/UDP ports, proxy config and public-IP routing; document overlap-free desired ports.
- Verify CPU/RAM/disk and traffic usage with non-invasive commands before choosing TURN relay range.
- Preserve existing network rules and OpenShock containers untouched.
- Stage config templates with **no real secrets in ZIP**; generate unique secrets on target when installation is actually authorized.
- After commissioning, compare OpenShock health and endpoints to pre-change observations, then run isolated rendezvous/TURN tests.

## Backup/evidence

Back up only new app configs with clear user intent; exports must not contain identity private key/remembered passwords by default. Before code or server changes, record version/hash. After controlled checks, publish evidence via canonical SMB path and verify independent SHA256 before sidecar last — this has not been done for the v1.0 documentation pack.
