# 16. Remote API conformance — approved test plan (not executed)

**Not executed**; normative **test specification**, not a test result. Existing upstream Windows M0 137/137 PASS as separately documented. Tests must not actuate real hardware.

## Wire/DTO (minimum)

1. Schema-validation for **all** `command` variants from `06-command-matrix.md`; also intentionally invalid request for each command; no `CheckUpdate` / `StartUpdateDownload` / `SkipUpdateVersion` / `DismissUpdate` exposure.
2. Round-trip type completeness: every field in `Snapshot`/`CliConfig`/`ChannelConfig`/`CliStatus`/`ZoneEvent`/`AlarmConfig`/`AlarmStatus`/`PresetEntry`/`ZonePresetEntry`, including default-suppressed fields, test via explicit Host fixtures. Reject silently dropped, swapped or rounded critical values.
3. DTO fuzz: oversized JSON, invalid Unicode, nested `Compose`, NaN/Infinity (invalid JSON), negative indices, `segment=4`, invalid channel, unknown extra command args, unknown op, duplicate request_id + changed payload.
4. `command.accepted` only if sender enqueue reports success; closed channel => `UNAVAILABLE`; never promise applied. Hardware output beyond remote cap should be displayed unchanged.
5. Concurrent Host-local switch, remote indexed zone edit, catalog refresh: no crash and no unsupported atomicity promises. If optional selection precheck fails, return `STALE_SELECTION`; if precheck passes but Worker changes later, only authoritative Snapshot reveals final result.

## Auth, session, security

6. Login valid/invalid, rate limit/backoff, unauth cannot view states, health minimal; password never in logs/URL/webview.
7. Two terminals (including one WSS and one RTC), both know password: second `SESSION_BUSY`, no eviction, Host egui still operates.
8. Liveness fake-clock: 5 s / ~15 s / 30 s after detected loss; 45 s nominal total; explicit logout is immediate.
9. Cross-transport WSS→RTC→WSS resume with one `session_id`; old generation cannot issue commands; no second slot.
10. Lost `auth.resume` ACK followed by same tuple replay: exactly one generation increment/one new ticket; changed `request_id` with consumed ticket rejected; 15 s bounded cache expires (ADR-030).
11. Host `Disconnect client`, password change, Remote OFF, Host process restart: all relevant access credentials invalid; terminal stops background login after host_kick and waits for explicit user action.
12. Access token expiry/refresh after 900 s (ADR-030): preserve active operator session without creating duplicate; revoked token cannot access REST.
13. Cert renewal same pinned Host key accepted; changed Host key requires manual re-verification; tampered RTC fingerprint/signature rejected before secrets transmitted.
14. TURN/signaling malicious alteration cannot impersonate pinned Host; no built-in STUN/TURN/Signaling URL on fresh install.

## Remote cap A/B

15. Master OFF, saved caps A=70/B=120: remote `A.max=200,B.max=200` allowed under upstream range.
16. ON A=70/B=120: A max/min=71 rejected; B max=100 accepted, 121 rejected; local Host max=180 displays **180** and not 70/120.
17. Remote `config.load`, zones, presets, modulation, Intensity, alarm have **no** extra cap rule. They are not filtered by an absolute output governor.
18. A current min=80/max=120/cap=70: remote `set_limit_max(A,70)` rejected without enqueue because upstream would coerce max to 80. Lower min first.

## Transport and reliability

19. WSS and reliable ordered RTC transport produce equivalent normalized messages/commands; no independent logic branch.
20. Request command idempotency: no automatic replay after loss. Same request_id within 120 s replay window returns cached ACK if implemented; don't enqueue twice; expiry yields explicit guidance to resync.
21. Network snapshot coalescing doesn't change Worker tick; state changed Host-local promptly appears on Terminal, vice versa; gap triggers `state.get`.
22. Repeated reconnections, fragmented frames, lost ping/pong, DNS change, NAT switch, relay fail, ICE restart: never double-activate remote session.
23. REST-only client must maintain heartbeat, fail when slot FREE/RESERVED or stale token. Error schema and statuses match WSS/RTC.

## Example fixture set

See `examples/*.json`. Every file is an **approved protocol-design example**; example validity does not prove any implementation. Static validators confirm schema structure and representative fixtures only; they **do not** assert implementation exists.

## Release gates

- Windows Rust `cargo test --workspace` and new remote tests PASS on branch, no modification of `src-core` without separate approval.
- Windows Tauri E2E + Host direct LAN + off-LAN WAN/DDNS/UPnP and explicit manual forward; test failure reporting.
- ICE direct candidate + coturn relay on BYO publicly reachable server, private signaling; credentials never built into client.
- Evidence publish to `\\\<evidence-share>\Torrents\Codex\ShockingVRC-Term\...` readback SHA256 and `.sha256` last. No claim before doing it.
