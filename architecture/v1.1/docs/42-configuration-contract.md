# 42. Configuration hierarchy and defaults

**Status:** design-level config contract. Only properties already approved in v0.15 are normative. Actual TOML/JSON field names outside that API are PROPOSED and must be adapted in implementation with backward-compatible parsers. Example templates live under `deployment/` and `schemas/`.

## Non-secret sample defaults

```json
{
  "host": {
    "remote_enabled": false,
    "listen_port": 4318,
    "remote_limits": { "enabled": false, "channel_a": null, "channel_b": null },
    "traversal": { "enabled": false, "rendezvous_url": null, "registration_key_ref": null, "stun_urls": [], "turn_servers": [] }
  },
  "terminal": {
    "saved_hosts": [],
    "selected_host": null,
    "rendezvous_url": null,
    "invite_token_ref": null,
    "stun_urls": [],
    "turn_servers": [],
    "connection_mode": "auto"
  }
}
```

Host `channel_a/b=null` means not configured yet; first explicit enabling of limits requires the Host owner to choose valid 0..200 for both. No hidden numerical cap is automatically applied. `listen_port=4318` is inherited from approved Host/API design. The sample is a design projection, not an already implemented on-disk serialization.

## Secrets and files

| Config owner | Saved where (proposal) | Sensitive parts |
|---|---|---|
| Host policy | Windows per-user app config | Enabled status, listen port, caps; no secrets in logs |
| Host auth | Windows protected credential store / password hash store | Argon2id hash, salt; private identity key protected on Host |
| Host rendezvous registration | Windows Credential Manager / DPAPI as supported; config contains only `registration_key_ref` | Host-only key supplied by self-host operator; never in JSON plaintext |
| Terminal profile | Windows per-user app config | Host endpoints, pinned Host public identity, UX prefs |
| Terminal remembered password | OS Credential Manager/DPAPI opt-in | Never save plaintext into JSON/profile export |
| Terminal rendezvous invitation | OS Credential Manager/DPAPI opt-in; config contains only `invite_token_ref` | Per-Host invite token supplied by self-host operator; distinct from Host password |
| Terminal live ticket/token | Native Rust process memory | Never persisted; lost on restart |
| Rendezvous operator config | Linux host local file/env/secrets | Registration keys, room auth, TURN static secret |
| coturn | Linux host file/secrets | `static-auth-secret` corresponds to rendezvous issuer |

## Empty-by-default interpretation

`null`, absent, or empty collection means **not configured** and must not trigger background public network probing. Provide UI to set and clear each user-owned endpoint explicitly. Host may expose a direct listener after owner enables Remote Access without any traversal provider.

## Config precedence

One source of truth per process. Proposed precedence: validated explicitly supplied operator CLI/env values override config file **only for standalone Linux servers**; Host/Terminal app settings controlled through their own local UI, not untrusted incoming remote commands. Never permit remote mutation of Host credentials, listener, remote caps or long-term key through approved 37-op API.

## Examples are not credentials

`signal.example.net`, `203.0.113.10`, `<SET_...>` are placeholders. No names, ports or credential samples refer to a verified actual Moscow OpenShock deployment. Config with placeholder values must fail validation and refuse start rather than quietly contact an unrelated external server.
