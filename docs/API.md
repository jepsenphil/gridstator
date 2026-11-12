# Native controller API

The Rust v2 controller implements the accepted `/v1/...` wire protocol. The prefix names the wire contract, not the v2 product release. This page is an integration map, not an independent compatibility promise: exact request/response schemas and access rules are defined in [native/src/protocol.rs](../native/src/protocol.rs), [controller.rs](../native/src/controller.rs) and the [accepted parity fixtures](../native/contracts/accepted-v1). The [v1 API guide](legacy/v1/API.md) is archived separately.

## Transport and identity

All controller traffic uses HTTPS or WSS. A client verifies the cluster CA fingerprint obtained out of band, then uses a named client token. Workers use their own registration credentials and outbound channel. Administrative token, settings and release operations require an admin role. Do not send credentials to an unpinned endpoint or expose the controller to the public internet.

| Route family | Purpose |
| --- | --- |
| `/v1/health`, `/v1/cluster` | Health, identity and capability information |
| `/v1/nodes`, `/v1/agents/register`, `/v1/agent/ws` | Worker registry, capacity, outbound assignment and reporting |
| `/v1/blobs/*`, `/v1/snapshots` | Missing content, verified blob transfer and immutable snapshots |
| `/v1/jobs/*`, `/v1/groups/*` | Submit, inspect, cancel/retry and aggregate jobs |
| `/v1/events` | Live client events |
| `/v1/tokens`, `/v1/settings` | Authorized administration |
| `/v1/controller/build` | Verified native release import and managed update |

Use the native CLI for ordinary integrations: `gridstator --help`, `gridstator jobs --json`, `gridstator nodes --json` and `gridstator run --json` expose stable operational output without reimplementing certificate pinning, snapshot transfer and retries. The VS Code extension uses the native bridge for workspace operations.

## Compatibility and release status

The native server and clients negotiate capabilities and reject incompatible roles. v1 and v2 data directories, credentials and services are separate even where wire paths are shared. The v2 API remains a development preview until the [release gates](RELEASING.md) pass; accepted differences from the v1 reference are in [compatibility exceptions](../native/contracts/compatibility-exceptions.json).
