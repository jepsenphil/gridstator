# Controller update source acceptance

All sixteen `reference-controller-updates-001`–`016` cases have explicit native policy/control
assertions in [updates_source_scenarios.rs](../../../tests/updates_source_scenarios.rs). The final
focused run passed 36 tests: 20 new update scenarios, 10 existing update contracts and six exact
controller source scenarios. The real-network act override test passed separately. Strict scoped
Clippy and rustfmt passed. `evidence.json` records exact commands, row/test mappings, exceptions,
source hashes and environment; `source-red.log` preserves the ten failures reproduced before fixes.

| Source row suffix | Native proof |
| --- | --- |
| `001` | Current/behind/missing-build/unmanaged/offline states, capability, runtime metadata and last-reported diagnostics |
| `002` | Native unbundled replacement: absent verified archives makes the controller unavailable for worker updates, including an administrator request |
| `003` | API default drain deadline is null; injected eight-day advance preserves work and sends one check only after work finishes |
| `004` | Sequential two-worker reducer and scheduler holds; actual route offers remain unavailable while draining, then only one worker gets a candidate until confirmation |
| `005` | One cancellation effect; real TLS/WebSocket start-update request sends job.cancel and only nudges update after a cancelled result |
| `006` | Active failure report, node-prefixed halt, immediate failed/pending projections, automatic suspension and explicit retry; unrelated pending reports do not halt the active worker |
| `007` | Launcher rollback facts halt the rollout, mark failure, restore scheduling and preserve the source diagnostic |
| `008` | Injected update deadline halts without advancing the next worker |
| `009` | Explicit cancellation halts, restores scheduling and immediately projects behind |
| `010` | Default follow mode creates an automatic rollout |
| `011` | Actual TLS API rejects window mode without a window; injected closed window blocks inherited policy while a node follow override proceeds |
| `012` | Update-only bootstrap without an agent socket for all four native targets; manifest/archive hashes, native envelope, mirrored artifact bytes and completion |
| `013` | Normal and midnight-wrapping windows preserve inclusive start/exclusive end boundaries |
| `014` | Managed act drift affects current status; verified per-target mirrored archives, act-only view and completion after managed tool metadata returns |
| `015` | Override/off view and drift; real act 0.2.96 upstream download/checksum comparison, extraction and executable version probe on Linux x64 |
| `016` | Unmanaged workers on the current native build ignore managed-act drift |

The native replacements are deliberate approved differences: native manifests/target/archive
digests replace npm entry-point identity; host Node runtime management is removed; the toolchain
contains act only; Darwin has managed-act offers rather than the old unsupported-platform empty
result. The tests assert those replacement fields and exclude legacy Node/entry metadata.

The real override archive was 8,484,270 bytes, SHA-256
`b861e901c9062d841c6e8ecd36cdf27e0ce39ea0124cfd93de6efcebea0e16e7`.
Its extracted executable SHA-256 was
`4778494a4f0128828958bdc92828e7bac2ddeb003b640b19ac41b103a4ca433f`, and it reported
`act version 0.2.96`. The independently fetched public checksum document and metadata are retained.
No Docker workload or service was started by that probe.

Default control fixtures have synthetic executable headers that the new tests never execute.
They establish policy, routes and identity/digest contracts. Actual Linux worker installation,
controller schema/crash rollback and actual M4 worker installation are linked in `evidence.json`,
with their own exact payload/source checkpoints. They are separate runtime evidence; they do not
make every target in a four-target fixture an executed target. In particular, the M4 package proof
uses act off and has no Docker readiness evidence.

This is bounded local source acceptance, not a claim of final release provenance, mixed-target
LAN rollout, Intel Mac/WSL2 runtime, Mac act/Docker, signing, launchd or reboot acceptance. The
working tree remains uncommitted. Credentials, private keys, databases and downloaded executables
are excluded. Preserved files were scanned for credential-like token strings; `SHA256SUMS`
verifies this evidence set.
