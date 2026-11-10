# Native v2 implementation progress

Date: 2026-11-10. The [confirmed Rust plan](V2_UPGRADE_PLAN.md) remains the baseline.
The CLI, controller and worker are Rust executables. The thin TypeScript extension uses
VS Code's supplied Node runtime. Native clusters use fresh credentials, separate
`gridstator-v2` state and separate services; the retained TypeScript core remains a reference.

The [parity ledger](../native/contracts/parity-ledger.json) records **223 of 228 rows verified**.
Five aggregate platform/service/signing gates remain open. These are bounded acceptance
counts, not engineering-effort or stable-release readiness percentages.

The baseline preview is `native-preview-20261110-6994d2cc804b`, version `2.0.0-dev.1`.
Its [40 production/build inputs](../native/contracts/evidence/preview-parent-2026-11-10/source-checkpoint.json)
have checkpoint SHA `6994d2cc804b876d34375a633f0f0dbf60cdacf261ca78bf02c5245e5ec11b65`.
Four-platform rebuilding and final-byte validation passed. The unpublished delivery contains the
[native bundle](../native/release/native-preview-20261110-6994d2cc804b/gridstator-2.0.0-dev.1-native-bundle.tar.gz),
[per-target downloads and installer instructions](../native/release/native-preview-20261110-6994d2cc804b/README-preview.md)
and [native VSIX](../apps/vscode/dist/gridstator-native.vsix).
All **195 distribution checksum entries** pass independent verification. The
[assembly and installer receipt](../native/contracts/evidence/preview-parent-2026-11-10/assembly/summary.json)
records a private-prefix installation, exact final checksum download, TLS startup, snapshot upload,
encoded-blob read-back and cleanup. The native bundle SHA-256 is
`b22052eaeabfd7d8a498c2958638c22e44c960e1e689e27c847cf4d43b6a350c`.
Earlier preview directories and their native bytes/checksums remain historical; their subsequently
discovered gaps are recorded.
The working tree is uncommitted: embedded `sourceRevision` is the parent reference commit,
not clean native-release provenance. Actual code is identified by checkpoint, immutable snapshot
and artifact hashes. Mac development binaries are unsigned/ad-hoc, without Developer ID notarization.
No public release or upgrade of the existing production v1 cluster has occurred.

The current feature preview is `native-rename-20261110-f0b1636fbd91`, version `2.0.0-dev.2`.
It adds durable worker renaming through `nodes set --name`, the node PATCH API and the editor's
Nodes/Cluster controls. Identity, credentials, settings and historical job names are preserved;
stored job targets/preferences are rebound to stable IDs so queued work and retries survive a rename.
Workers persist the authoritative name from welcome/settings frames without a service restart.
Old controllers that ignore the new field are reported as incompatible rather than as a successful
rename. [Verification evidence](../native/contracts/evidence/worker-rename-2026-11-10/README.md)
records 313 passing TypeScript tests (10 skipped), 26 native contract checks, two native name-sync
unit cases, 29 browser tests and opened light/dark wide/narrow captures.

The native v2 cluster and its account CLI were updated through the verified managed release flow.
`DESKTOP-JEL` was renamed to `big-box` with the same node ID and uninterrupted connection. A real
Docker job targeted by `big-box` succeeded and printed that name through `GRIDSTATOR_NODE_NAME`.
Both workers and the controller report the new build. The updated native VSIX was installed in the
active SSH editor host; existing windows need a reload to load it. The new
[bundle](../native/release/native-rename-20261110-f0b1636fbd91/gridstator-2.0.0-dev.2-native-bundle.tar.gz),
[source checkpoint](../native/release/native-rename-20261110-f0b1636fbd91/SOURCE_CHECKPOINT.json) and
[delivery notes](../native/release/native-rename-20261110-f0b1636fbd91/README-preview.md) remain local
development artifacts. Linux ARM64 was cross-compiled, and macOS build-info was run on Apple Silicon
(Intel via Rosetta); this does not close the remaining platform/signing acceptance gates.

## Implemented behavior and reference coverage

The implementation covers CLI/configuration, native administration, TLS/authentication, SQLite,
snapshots/CAS, scheduling, commands/builds/Actions/shards, logs/artifacts/JUnit, durable worker
outboxes/recovery, installation/supervision, managed updates and the editor adapter.

The frozen v1 reference is commit `4b8f13ccf1b21820007fef0ab52143c961b69216`.
Its 190 source digests and original fixtures are preserved in the
[accepted baseline](../native/contracts/accepted-v1/baseline.json). An AST audit matches all
**203 frozen test declarations**, including conditional and parameterized cases. Two initially
missed `it.skipIf` cases were added as integration012 and agent-updater003 without changing
existing IDs or frozen bytes. Additional blob-URI and config-filesystem boundaries have explicit rows.

- [CLI/protocol](../native/contracts/evidence/cli-protocol-acceptance.json): 56 help paths,
  253 parser processes, 35 whole-program workflows, schemas/errors/exit boundaries,
  local administration, seven release and seven update-compatibility scenarios.
- [Workspace](../native/contracts/evidence/workspace-acceptance.json): canonical manifests,
  exact capture, unsafe paths/links/archives and 18,926 supplemental matcher comparisons.
  Lookaround/backreferences use a bounded Rust backend; arbitrary unbounded-regex equivalence
  is not claimed.
- [Controller/runtime](../native/contracts/evidence/closure-2026-11-10/evidence.json): all twenty
  originating controller cases, differential scheduling, ownership/assignment fencing,
  cancellation, live logs, artifacts and bounded archive transfers. Disconnected completion
  replays split UTF-8 and the final partial line before result acknowledgement/outbox cleanup.
- [Executors/agent](../native/contracts/evidence/executor-agent-2026-11-10/acceptance.json)
  and [two-worker integration](../native/contracts/evidence/integration-source-2026-11-10/README.md):
  actual Docker/buildx/act, cancellation, cleanup, outputs, twelve frozen CLI integration cases,
  controller reconnect, owned-resource recovery, automatic retry and sharded reports.
- [Updates](../native/contracts/evidence/controller-updates-2026-11-10/evidence.json)
  and [installation](../native/contracts/evidence/install-source-acceptance.json): originating
  update/install scenarios, physical running-payload identity, healthy supervisor refresh,
  offline recovery and same-ID mirror/bootstrap collision rejection before mutation.
- [Managed act](../native/contracts/evidence/worker-toolchain-source-2026-11-10/README.md):
  a native worker with empty PATH installs upstream-verified act, reports its managed version
  without changing core PID, and preserves the previous tool/core/launcher through repeated
  invalid-candidate failures. Removed host-Node assertions have explicit native replacements.

## Corrections verified during actual-host work

[Encoded blob GET](../native/contracts/evidence/blob-client-encoding-2026-11-10/README.md)
now decodes the retained SDK's URI-encoded digest once before strict validation. Actual TLS
binary-byte checks and the retained TypeScript SDK pass; malformed/double-encoded paths remain rejected.

[Buildx image metadata](../native/contracts/evidence/buildx-local-reference-2026-11-10/README.md)
now handles both missing config digests and config digests that a newer containerd store cannot
inspect. Local load/tar modes try at most two distinct metadata references, keeping an addressable
config digest first and using the manifest/index when necessary. Remote/cache-only semantics
remain unchanged. Five units, four real Linux builds and four real M4 builds pass. The complete
[strict native ARM workflow](../native/contracts/evidence/linux-arm-vm-workflow-2026-11-10/README.md)
passes on the corrected source, including real Actions, two-worker shards and reports.

[Darwin host memory](../native/contracts/evidence/m4-memory-docker-2026-11-10/README.md)
uses checked Mach free+inactive pages with physical/overflow bounds and actual host page size.
Both initial and heartbeat metrics use it; Docker VM capacity remains distinct. Native/Rosetta
readings agree within a few MiB with nearby OS observations, including the older-kernel struct
prefix. The same worker remains connected through Docker stop/start and completes queued work
after readiness reprobe. [Eight actual M4 worker cases](../native/contracts/evidence/m4-worker-docker-2026-11-10/README.md)
cover signals, drain, durable results, disconnection replay and update admission.

[Generic config writes](../native/contracts/evidence/config-parent-permissions-2026-11-10/README.md)
now preserve existing parent permissions and support relative leaf filenames, matching frozen v1.
Missing parent components are created `0700`; config/lock files remain `0600`; explicit dedicated
state-directory enforcement remains unchanged. The owned-temp baseline reproducer, regression
reds/greens, two state properties, all ten CLI cases and independent review are preserved.

## Actual host, editor and recovery evidence

[Local Linux editor](../native/contracts/evidence/closure-2026-11-10/evidence.json),
[Mac Remote SSH](../native/contracts/evidence/remote-ssh-m4-2026-11-10/README.md),
[three actual concurrent SSH windows and five unsaved-buffer choices](../native/contracts/evidence/real-windows-unsaved-m4-2026-11-10/summary.json)
and [Windows Remote WSL](../native/contracts/evidence/remote-wsl-2026-11-10/README.md)
exercise the native-only VSIX. WSL uses the exact final extension bytes
`d0cafff141d2ca5e318a0bc0633150f2ce896e356a399bea92b44dec7ba35eb6` with an actual Linux/x64
workspace extension host: paths with spaces, live Docker logs, exact artifact content,
VS Code Testing, cancellation and missing/incompatible helper errors without fallback/new jobs.
Shared-login models additionally cover the deliberately slow managed update. Receipts name their
exact native checkpoints; historical editor runs are not relabelled as latest-core byte tests.

[Native WSL](../native/contracts/evidence/wsl-native-2026-11-10/README.md) verifies CLI/controller,
standalone/embedded workers, Docker, process/service restarts and five actual Windows-to-WSL
private-CA TLS health observations. Its owned user unit uses `sg` for already-granted Docker
membership absent from the stale manager and control-group stop for that extra parent. These
fixture adaptations do not prove an unmodified default unit succeeds in a stale session.
The manager, groups, socket and legacy agent were preserved. No stopped-distro/Windows-boot
startup guarantee is inferred.

[One-cluster direct-LAN rollout and system recovery](../native/contracts/evidence/mixed-lan-system-2026-11-10/README.md)
uses actual Linux x64 and M4 managed workers, genuine four-target positive bundles and a unique
root/systemd unit. Both workers update sequentially, confirm exact hashes and execute real jobs.
An actual controller candidate adds a table/column, sets schema version999 and erases jobs;
after three crashes the supervisor restores the confirmed executable, database, history and CA.
The negative fixture's other-target headers are declared fixtures and were not offered to workers.
The existing v1 agent's PID/invocation/unit bytes remained unchanged; all owned actors and data were cleaned.

The final common-byte [Linux self-build](../native/contracts/evidence/self-build-parent-preview-2026-11-10/README.md)
submitted job `01M724AVW456E5EY8W9BJ824T7` through Gridstator itself. Its immutable snapshot,
1,174 source-manifest entries, forty core inputs and three supplemental inputs agree. The final
static x64 binary passed clean, read-only, offline Alpine TLS and config-mode checks. The exact
ARM binary passed all six strict Docker workflow cases and the three config-mode cases in the
M4's native ARM Linux VM, plus a separate [unprivileged clean runtime](../native/contracts/evidence/preview-parent-2026-11-10/arm-clean-runtime/README.md).

[Final Mac foreground/user-LaunchAgent checks](../native/contracts/evidence/mac-preview-parent-mode-2026-11-10/README.md)
run ARM natively and Intel under Rosetta; both exact executable payloads passed all three config-mode
cases, and ARM passed actual Docker/artifact/cancel/build/encoded-GET behavior. Docker Desktop
4.99.0 was installed from Docker's published-checksum download and accepted as notarized by
Gatekeeper; that verifies the external Docker application, not Gridstator signing. Actual native
Linux ARM execution occurs in the M4's ARM Linux Docker VM, without x86 emulation. It does not
establish a standalone ARM systemd/boot lifecycle. Distribution assembly and installer evidence
remain distinct from the still-open signed stable-release qualification.

## Checks and remaining release gates

The [final coherent corrected-source suite](../native/contracts/evidence/preview-parent-2026-11-10/quality/summary.json)
passed **250 native tests, zero failures and 32 explicitly ignored external-environment cases**
across 43 targets. Strict all-target Clippy, Rust/repository formatting, the frozen AST inventory
and structural ledger validation passed. Stable release validation fails as expected for the
five rows below.
The retained Node suite passed 194 tests with ten opt-in skips; the added actual SDK case passed
separately. Typecheck/lint/format and twenty Playwright cases passed. Explicit actual-host suites
are recorded separately, and overlapping runs are not summed.

Earlier parallel archive timing and shared-debug-binary build-race failures remain in the
[historical quality receipt](../native/contracts/evidence/preview-darwin-2026-11-10/quality/summary.json).
The unchanged one-second archive bounds passed in isolation and complete coherent serial runs;
shared Cargo builds are serialized during identity-sensitive tests. No production timing bound
was weakened to obtain a pass.

| Open aggregate gate | Evidence now available | Still required before stable |
| --- | --- | --- |
| Mac signing | Pipeline, unsigned/ad-hoc development artifacts | Apple credentials; Developer ID signing/notarization and downloaded-byte verification on both actual architectures |
| Four-target runtime/linkage | Four real builds, clean x64/ARM execution, native M4 and Intel under Rosetta | Physical Intel and complete clean/minimum-OS/install/recovery matrix |
| Mac Docker capacity | M4 host/VM separation, actual executors, readiness and recovery | Physical Intel Docker VM limits/placement/recovery and remaining controlled placement cells |
| launchd lifecycle | Actual unique user LaunchAgents on both executable architectures; separate M4 Docker recovery | System LaunchDaemons, login/logout/reboot and late-Docker service lifecycle on actual Intel/ARM Macs |
| systemd lifecycle | Actual Linux x64 user/system and explicitly adapted WSL user-service workflows | Actual Linux ARM64 user/system manager lifecycle; container foreground execution is insufficient |

Stable release also requires clean native source provenance and the owner's release/cutover decision.
No hosted CI run is inferred from workflow definitions. The original Mac/Linux accounts require a
password for noninteractive sudo; no privileged Mac daemon, reboot or user-session termination was
attempted. No usable Apple signing identity was available in the read-only SSH probe.

Run `node native/scripts/check-ledger.mjs` and
`node native/scripts/check-source-inventory.mjs FROZEN_CHECKOUT`. The ledger's `--release` mode
must remain failing while these five rows are open. See [TESTING.md](../native/TESTING.md),
[VALIDATION_HOSTS.md](../native/VALIDATION_HOSTS.md), [RELEASING.md](../native/RELEASING.md)
and [CUTOVER.md](../native/CUTOVER.md).

## Test isolation incidents

An earlier window-model test briefly changed an existing v1 laptop worker from enabled to draining
for **489 ms**, then restored it in `finally`. A release lookup returned404; no installation,
restart or job events occurred. A read-only audit found all three workers enabled. The
[incident and strengthened guards](../native/contracts/evidence/native-window-isolation-2026-11-10/README.md)
are retained.

An early empty Windows Code diagnostic opened the existing shared-storage database despite a
private profile. Zero writes are not guaranteed, and no global database reset was attempted.
No Gridstator extension/backend action was involved. Later guarded tests isolate client/server
storage and secrets; original desktop processes and legacy agent were preserved. The
[diagnostic and cleanup receipt](../native/contracts/evidence/remote-wsl-2026-11-10/diagnostic-README.md)
records that boundary explicitly.
