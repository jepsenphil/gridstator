# Conditional managed-tool updater replacement

`reference-agent-updater-003` now has an actual Linux x86_64 replacement proof for the
conditional source scenario at `apps/agent/src/updater.test.ts:232`. The ignored
`worker_toolchain_source.rs` test passed in 54.31 seconds; scoped strict Clippy and rustfmt
passed. No production code changed for this test.

The actual native controller and supervised worker start with an empty private PATH and
Docker explicitly unavailable. Host act, Node and npm are unavailable to those roles. The
worker starts from a verified installed native executable and reports a missing-act
toolchain gap. Its controller downloads real act 0.2.96; an independent upstream checksum
document agrees with the mirror. The worker installs it, executes its version probe and
reports `tools.act: "0.2.96"`, `managed: ["act"]`, `runtime: "rust"`, and the correct native
core build ID and executable digest. The toolchain view contains only act.

- Archive SHA-256: `b861e901c9062d841c6e8ecd36cdf27e0ce39ea0124cfd93de6efcebea0e16e7`.
- Executable SHA-256: `4778494a4f0128828958bdc92828e7bac2ddeb003b640b19ac41b103a4ca433f`.
- Actual version output: `act version 0.2.96`.
- Native payload PID: `1489189` before installation, after reprobe and after three failures.

The next offered act version, `9.9.99`, is an explicit synthetic negative fixture: its
archive digest and size are correct, but its `act` member is 64 zero bytes. Three requested
checks independently report `executable target unsupported does not match linux/amd64`
and halt the rollout. The same native process remains alive; the working act executable
and persisted tool identity remain unchanged; scheduling returns to enabled; no invalid
executable becomes installed; native core activation remains current with no pending
activation or rollback. The stable supervisor bytes also match the verified core digest.

The original test's Node 9.9.2 path, Node-specific install-state fields and failed Node
switch are replaced under the approved `native-no-node-runtime-management` and
`native-artifact-and-update-identity` exceptions. Act-only changes reprobe the native
worker instead of creating a core activation. Verified native identity/health-based
launcher refresh and failed-candidate safety replace Node launcher switching. The full
assertion-by-assertion map is in `evidence.json`; it also links the separate negative
identity tests and actual native core crash/schema-rollback proof. These distinctions are
explicit substitutions rather than claims that Node behavior remains implemented.

`worker-toolchain-source.json` records the initial missing-tool report, installed metadata,
process and connection identities, completed rollout, all three failure views and final
activation. The checksum document and sanitized controller/worker logs are retained.
Credential-free executable/tool copies remain under ignored
`native/target/worker-toolchain-source-artifacts/linux-amd64/`. Private configuration,
credentials, keys and databases were removed after both owned role processes stopped.
`SHA256SUMS` covers the saved evidence.

To repeat on a disposable Linux host with upstream release access:

```sh
GRIDSTATOR_TOOLCHAIN_SOURCE_EVIDENCE=/tmp/gridstator-toolchain-proof \
  cargo +1.99.1 test --manifest-path native/Cargo.toml --locked \
  --test worker_toolchain_source -- --ignored --nocapture --test-threads=1
```

The core mirror's host member is the actual native executable; its other three members
are header fixtures never installed or executed. This is not a four-target release,
macOS act readiness, ARM runtime, OS-service/reboot or signing/publication proof.
