# Native integration source scenarios

All twelve scenarios in the frozen `tests/integration/e2e.test.ts` have bounded passing
Linux x86_64 evidence here. They use actual native CLI/controller processes, two separate
Docker worker processes, private loopback TLS state, and real Docker/buildx/act. No existing
services or clusters were changed.

This is an explicit matrix of two immutable executable checkpoints, rather than a claim
that one final release passed a single twelve-scenario command:

| Rows (`reference-integration-e2e-`) | Evidence | Result |
| --- | --- | --- |
| `001`–`007` | `matrix-001-007.json`, `source-001-007-then-race-red.log` | Each listed scenario passed; the overall command then failed at `008` and exposed a controller race |
| `008`–`011` | `matrix-008-011.json`, `source-008-011-green.log` | All four selected scenarios passed after the race fix, in 64.38 seconds |
| `012` | `matrix-012-actions.json`, `source-012-actions-green.log` | The conditional Actions scenario ran with act available and passed in 45.74 seconds |

`012` appends the source's conditional Actions test without renumbering the existing eleven
rows. The original extractor omitted its `it.skipIf(!actAvailable)` declaration. The exact
source assertions now include workflow discovery, the uncommitted greet test, successful
`Run tests` output, the precise artifact name, and a downloaded ZIP. Its small report is
preserved as `actions-test-report.zip`.

The worker-death scenario first observes a running owned container and workspace volume,
then kills and restarts only its recorded worker PID. It requires both resources to be
removed, a real container and volume labeled for another node to survive, and automatic
retry attempt 2 to succeed. Together with `007`'s controller interruption and exact twelve
log lines, this verifies `agent-restart-owned-cleanup`. The broader `log-result-ordering`
row still needs its separate partial UTF-8, completion-through-disconnect and terminal-ack
proofs; `007` alone does not establish those assertions.

The real crash scenario exposed a stale same-socket hello inventory replacing an in-flight
assignment. `retry-race-audit.json` preserves the ordered durable events; it explicitly
labels hello placement as inference, since the original run did not capture wire frames.
Two deterministic TLS/WebSocket regressions then failed in 0.57 seconds. After the narrow
controller fix, three regressions passed in 1.28 seconds: unread assignment identity and
resource reservations survive, running work survives, and pending cancellation is not
prematurely finalized. Frozen v1 workers send hello only when connecting; native capability
and admission refreshes reuse the same socket. Negative reconciliation therefore remains
authoritative on the first hello of a new connection, while later refreshes apply positive
observations and metadata. Existing reconnect tests cover the new-connection behavior.

Additional red logs preserve the CLI delimiter, terminal/image summary, duplicate-upload
diagnostic and worker recovery diagnostic defects fixed before the matrix completed.
The final scoped strict Clippy check passed. Both new test files pass rustfmt.

`evidence.json` records exact commands, row mappings, limits, binary hashes and checkpoint
differences. `binary-*.json` reports the executed build identities; their hashes were also
checked independently. The two copied executables are retained under the ignored
`native/target/integration-source-binaries/<sha256>/gridstator` directories. The earlier
one intentionally retains the exposed race and is historical evidence. Source digest
checkpoints describe an uncommitted working tree, not final release provenance.

`cleanup-audit.json` independently checks that all recorded role processes and their owned
containers/workspace volumes are gone. Existing Docker resources and shared caches were
preserved. Configuration files, tokens, private keys and databases are excluded from this
directory; logs and the ZIP contents were scanned before preservation. `SHA256SUMS` covers
the saved evidence.

To run the current complete source suite on a disposable Linux host with Docker/buildx,
git and act available:

```sh
GRIDSTATOR_KEEP_INTEGRATION_SOURCE_EVIDENCE=1 \
  cargo +1.99.1 test --manifest-path native/Cargo.toml --locked \
  --test integration_source_scenarios -- --ignored --nocapture --test-threads=1
```

These results establish runtime behavior on one Ubuntu x86_64 host. They make no macOS,
WSL2, multi-host LAN, OS-service/reboot, signing, notarization or publication claim.
