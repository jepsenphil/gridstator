# Exact controller source scenarios

The new [controller_source_scenarios.rs](../../../tests/controller_source_scenarios.rs) runs real
native loopback TLS/WebSocket listeners, with private temporary controller databases and scripted
agents. It starts no Docker workloads or services. It contains separate fixture helpers so that
importing another integration test file cannot duplicate its tests.

Six tests passed in 2.24 seconds; scoped strict Clippy and rustfmt passed. `evidence.json` records
the commands, environment, source hashes and exact row/test mapping. The full controller restart
scenario first failed on the frozen diagnostic; `source-013-red.log` preserves that failure. The
only production change in this handoff restores that public diagnostic literal. The complete
green run is in `source-scenarios-green.log`.

| Verified row suffix (`reference-controller-controller-`) | New exact scenario |
| --- | --- |
| `006` | Queued immediate cancellation, running cancel frame and failed exit 143 coerced to cancelled |
| `008` | Failed automatic retry and exhaustion, manual attempt 3 with lineage, active retry conflict |
| `012` | One disappearing worker simultaneously loses a running job and requeues an assigned job |
| `013` | Actual controller restart; known active resume, forgotten job lost, unknown active cancellation, durable log position and replay |
| `014` | Disconnected pending result in welcome.resume, later result completion and acknowledgement |
| `018` | Two-node shard spread and substitutions, mixed completion aggregation, failed-only retry assignment and lineage |

The shared working tree is not an immutable release candidate. These are bounded controller
protocol/API proofs, without target-platform/service/reboot or broader editor claims. Logs were
scanned for credential-like token strings before preservation; no credentials or databases are
included.

The independent review also checked the retained v2 editor seams. The unchanged Problems matcher
tests and sidebar-provider tests can verify their respective pure/component rows. The current
20-test Chromium dashboard run can verify rendering and posted-message rows, including native
Rust metadata display; its stubbed `acquireVsCodeApi` does not exercise a native backend. Existing
settings/state tests instantiate the old Node controller and model windows within one process,
so they do not verify native update/connection scenarios or actual SSH/WSL/multiple editor windows.
The separate green packaged-editor proof covers one actual local Linux editor window only.
