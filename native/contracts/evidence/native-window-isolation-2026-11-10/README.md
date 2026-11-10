# Native window test isolation incident and guard review

On 2026-11-10 an incorrectly scoped new integration test used the pre-existing v1 shared login. The settings test was outside the private fixture's `describe` hooks, so the configured private native login had already been reset. This was a test isolation error.

The selected controller/agent journal records establish that the existing `laptop` worker changed from enabled to draining at 02:29:08.921 UTC and returned to enabled at 02:29:09.410 UTC, an interval of 489 ms. The source settings action's `finally` restoration made that second change. The attempted rehearsal release lookup returned GitHub HTTP 404; no release was installed and the running v1 controller process did not restart. A read-only follow-up observed all three existing workers enabled and no active jobs. There were zero persisted job events during the audited 02:29:08–02:29:10 UTC window. No unrelated state was manually reset after discovery.

## Retained evidence

- `incident.json`: structured operations, exact journal timestamps, scope and original-log limitation.
- `v1-relevant-journal.ndjson`: selected original journal records containing the drain, failed release lookup and automatic restoration. No credential/configuration values are included.
- `live-after-readonly.json`: sanitized read-only follow-up cluster/build and node scheduling observations.
- `test-followup-paused.log`: full subsequent isolated-fixture run, which completed six cases and failed two before all further runs were paused.

The first run's redirected log was overwritten by the subsequent isolated run before the scope error was identified. Its printed failure remains in the session transcript, but its complete original log is unavailable. This directory does not present a reconstruction as an original log.

## Fail-closed guard and independent review

The guard is in `apps/vscode/src/native-windows.integration.test.ts`. A hoisted file-wide `@gridstator/client` wrapper refuses construction, URL retargeting, requests and WebSockets until a private native fixture is active. It permits only exact loopback HTTPS origins registered from that fixture's private CLI-generated client and controller configuration; the expected cluster ID comes from that fixture's private controller configuration and is checked on every cluster response. No existing/default login URL is allowlisted.

The active policy requires native mode and the exact private shared configuration path. Fixtures, model actions and actual NativeCli callbacks assert this policy; each settings action additionally probes and verifies the selected private cluster before it proceeds. Native child environments set explicit private controller/agent/client paths and clear inherited URL/token/CA/install identity overrides. The two original state cases and five settings scenarios remain inside the guarded hooks; ready and expired restoration are separate cases. The worker peer is a scripted protocol model, not a real executor.

An always-on negative regression refuses both an existing-looking LAN controller and the default loopback port before constructing a transport. The opt-in regression additionally invalidates an already-created private client, requires HTTP/WebSocket rejection, restores the exact private fixture registration and proves real private HTTP/WebSocket operation continues. It also rejects a configuration-path change. The parent independently reviewed the exact client callsites and approved the guard before any runtime retry. The complete action, including its cluster preflight, is retained during cleanup.

Scope: component models plus a real private Rust backend and actual native CLI where needed. These tests do not create actual SSH/editor windows, and do not replace the separate packaged VS Code host proof. The approved negative/real-private-continuation regression passed before the private model cases resumed; its retained log is `guard-approved.log`. The later complete acceptance run and exact source/binary identities are recorded separately in `../native-window-models-2026-11-10`.
