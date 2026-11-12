# Native controller API

The Rust controller implements the `/v1/...` wire protocol. The route prefix versions the wire contract independently of the product release. Exact request/response schemas and access rules are defined in [protocol.rs](../native/src/protocol.rs), [controller.rs](../native/src/controller.rs) and the [behavior fixtures](../native/contracts/behavior/).

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

The native server and clients negotiate capabilities and reject incompatible roles. The API remains a development preview until the [release gates](RELEASING.md) pass.
