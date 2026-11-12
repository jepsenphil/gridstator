# Native validation

Run commands from `native/`. The default suite needs Rust, git, and a host C compiler (`cc`); it does not need Node,
Docker, buildx, or act:

```sh
cargo test --all-targets
```

For a full acceptance run on a shared host, use
`cargo test --locked --all-targets -- --test-threads=1`. The archive backpressure
case deliberately retains a one-second heartbeat/cancellation bound; concurrent
builds and test cases can add scheduling or disk latency. Preserve any failed
parallel run and verify the same case and full serial run without relaxing its bound.

Tests that create Docker workloads are explicitly ignored by that command. Their ignored status is
missing execution evidence, not a passing platform or release gate. Run the required tool suites
explicitly on each validation host that supplies their prerequisites:

```sh
docker version
docker buildx version
act --version
git --version
cargo test --test executor_contract -- --ignored --test-threads=1
cargo test --test worker_contract -- --ignored \
  --skip independent_managed_worker_installs_restarts_rejoins_and_unmanaged_worker_stays_put \
  --test-threads=1
cargo test --test cluster_e2e -- --ignored --test-threads=1
cargo test --test integration_source_scenarios -- --ignored --test-threads=1 --nocapture
```

The tools must be accessible to the test account. The executor and worker suites fail when a
required tool is absent or its daemon is unavailable; they do not silently skip. The fixtures use
`alpine:3.20` and `node:24-bookworm-slim`, with network access for uncached image pulls and Actions
downloads. Current validation uses act 0.2.97. Test containers and workspace volumes carry unique
job/node ownership labels; tests remove only resources they own. Shared dependency caches and
BuildKit caches may remain warm between runs.

The independent managed-worker package scenario is separate from that eight-case Docker worker
command. It requires two real native release packages prepared by the recovery rehearsal; set
`GRIDSTATOR_WORKER_UPDATE_FIXTURES` to that directory and explicitly select
`independent_managed_worker_installs_restarts_rejoins_and_unmanaged_worker_stays_put`.
A skipped or missing fixture does not prove the update path.

The worker suite starts isolated native controller/agent processes and temporary clusters. It
checks graceful drain, ordinary shutdown, coordinator cancellation, interrupted-job recovery,
disconnected completion and log replay, missing snapshots, artifact-upload failure, pinned endpoint
relocation, and configured cache eviction. The executor suite covers real commands, cancellation,
timeouts, artifacts, BuildKit stages/secrets/image export, and act workflows, reusable workflows,
artifacts, live steps, socket opt-in, and ownership repair.

`integration_source_scenarios` reproduces all twelve frozen CLI end-to-end scenarios with two
actual Docker workers. It includes concurrent placement, controller restart without lost logs,
worker death/owned cleanup/automatic retry, image builds and combined sharded JUnit results.
It records the immutable executable used by its private fixture; unrelated containers and volumes
must survive recovery. Run this suite serially without a concurrent self-build workload.

The default `managed_role_contract` starts actual native controller and worker roles under real
supervisors, verifies that healthy roles refresh their stable launcher bytes and checks truthful
managed identity. Docker is deliberately unavailable: a command remains queued until cancelled.
This is runtime-hook evidence, not a Docker, provisional-update or OS-service claim.

On Linux with a running systemd user manager and Docker, the isolated service proof is explicit:

```sh
cargo test --test systemd_e2e -- --ignored --nocapture
```

It installs a uniquely named user unit, starts a native supervisor/controller/embedded worker,
runs a real Docker command, restarts the controller and stops/removes the unit. It uses private
temporary state and checks graceful runtime-file cleanup. It does not prove system services,
reboot/login behavior, WSL2 or another architecture. `GRIDSTATOR_TEST_BINARY=/absolute/path/gridstator`
can select an already built native candidate; otherwise it uses Cargo's native CLI executable.

Service lifecycle, signed/notarized download, Mac Docker readiness/VM limits, mixed-target updates,
and rollback still require their separate evidence in [the native release plan](../docs/NATIVE_RELEASE_PLAN.md).
Cross-compilation and an ignored test do not replace those runs.

From the repository root, the editor adapter can exercise an actual built Rust helper/controller
through the retained Node client and its source response schemas:

```sh
GRIDSTATOR_NATIVE_CLI_E2E="$PWD/native/target/debug/gridstator" \
  pnpm exec vitest run apps/vscode/src/native.integration.test.ts
```

This creates a private cluster and workspace, tests discovery/capture/submission, typed API views,
groups, cancellation, native worker registration and local administration, then stops its processes.
It needs Node as a development/extension-host test; the native target itself still runs without it.
It performs no Docker workload. The default Node suite explicitly skips this case when that
executable-selection variable is absent. This does not replace actual SSH/WSL/VS Code host tests.

The complete packaged v2 extension can also run inside actual VS Code on a Linux desktop test host
with Docker and Xvfb (or an existing display):

```sh
pnpm --filter ./apps/vscode package:native
cd apps/vscode
GRIDSTATOR_NATIVE_CLI_E2E="$PWD/../../native/target/debug/gridstator" \
  xvfb-run -a node test/run-integration.mjs
```

This extracts and runs the packaged VSIX, copies the selected executable to private immutable test
state, creates a private Rust cluster/worker and checks discovery, JUnit/Problems/Testing, shards,
live group completion, cancel/retry and node controls. It deliberately retains a false native-mode
preference to verify that the v2 package always uses Rust. A successful Linux run does not close
Remote SSH, WSL2, Mac or concurrent-window acceptance. The script's existing optional external-cluster
mode is explicit; omit `GRIDSTATOR_IT_CONFIG` for isolated local validation.

Retained window component models can also exercise the actual native backend and a real managed
controller update, using the separately prepared Linux recovery packages:

```sh
GRIDSTATOR_NATIVE_WINDOWS_E2E="$PWD/native/target/debug/gridstator" \
GRIDSTATOR_NATIVE_WINDOWS_RELEASES=/absolute/path/to/recovery-packages \
  pnpm exec vitest run apps/vscode/src/native-windows.integration.test.ts --no-file-parallelism
```

The test file has a mandatory private configuration/cluster/URL fence, including negative
constructor, request and WebSocket checks. It models shared login/rotation/revocation, drain leases,
cluster changes and restoration through a deliberately slow actual update. Its scripted protocol
peer does not claim a Rust executor identity. These models supplement the actual editor-host proof;
they do not replace real Remote SSH/WSL or multiple running editor windows.
