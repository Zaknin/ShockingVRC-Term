# 46. Planned setup journey — Host owner and remote friend

**Status:** end-user workflow SPECIFICATION, not a currently working installation guide. The steps below describe future UIs and must not be mistaken for already available features.

## A. Direct LAN / VPN first use

**Host (Nightmare or another owner):**

1. Keep normal egui ShockingVRC running as before; open `Setup → Remote Access`.
2. Enable Remote Access; set a username/password. The listener defaults to project-selected TCP port (reference 4318) but is editable. No UPnP or public provider necessary on LAN/VPN.
3. View locally displayed Host ID/fingerprint. Communicate it to the intended Term user over a separate trusted channel; it is not a secret password.
4. Host sees `Listening` on correct local address and no active session.

**Terminal:**

1. Launch Tauri `Connect`; create Host profile, enter IP or DNS name and port. Don't prefill Moscow/signaling.
2. Confirm Host identity fingerprint matches the one owner gave. This is pairing, not password exchange.
3. Enter Host login/password; choose to store password in Windows Credential Manager only if desired.
4. On `ONLINE`, inspect Status first; all nine original egui sections available. Commands update Host and the next Snapshot updates both displays.
5. When finished, Logout releases the one slot.

## B. Direct over public WAN / DDNS

Host optionally enables UPnP/NAT-PMP/PCP or personally sets manual forwarding on router. Map chosen TCP listener; a DDNS hostname may be supplied instead of fixed IP. Host shows mapping **result**, not a guarantee of WAN reachability. Terminal uses the DDNS name and still verifies Host identity fingerprint. No VPN or external signaling dependency.

If upstream router/ISP uses CGNAT and inbound mapping cannot work, inform user and offer user-configured ICE/TURN (or VPN), do not pretend UPnP or STUN fixed the TCP forwarding.

## C. ICE/TURN through self-hosted Linux instance

**Operator (could be same person as Host owner):** follows `deployment/README.md`, inspects open ports first, sets up private rendezvous Rust binary and coturn with their **own** credentials. `server_id`/room invite are routing credentials, not a Host application password.

**Operator:** configures individual Host registration key and per-Host invite token via own rendezvous policy file/operator tooling; shares each value only with its intended user, out-of-band. No default credentials are shipped.

**Host:** enters user-owned rendezvous URL and **Host registration key** (as `register.authorization`), plus STUN/TURN URIs or obtains short TURN REST credentials via configured signaling; enables traversal. No endpoint or key is preloaded by the app.

**Terminal:** configures matching rendezvous URL and independently entered **per-Host invite token** (`join.authorization`) for this Host profile, establishes validated identity, logs into same Host SessionManager after DataChannel is verified. Invite token is distinct from Host application password and may be stored via OS credentials only with explicit opt-in. Selected path is shown as ICE P2P or TURN Relay. If relay fails, same Terminal presents diagnosis without touching local engine.

## D. Network interruption and conflict

- Second Terminal gets `SESSION_BUSY`; it cannot kick current Term.
- 5s heartbeat, loss detection ~15s, reservation 30s after detection; original Terminal can resume by ticket; full snapshot follows.
- Owner's `Disconnect Terminal` forces `REVOKED`, and saved credentials never trigger an automatic re-login after kick.
- Term seeing Host A.max above allowed remote cap shows actual value and explicit remote-settable boundary; it never silently clamps real Host display.

## E. No remote server configured

Blank rendezvous/STUN/TURN fields are valid default configuration. Direct mode remains the minimal working experience. End users are not required to create VPS, DNS account or cloud registration to use local/VPN WSS.
