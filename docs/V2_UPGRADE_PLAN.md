# Gridstator v2: native Rust upgrade plan

Date: 2026-11-09. Status: review draft; scope decisions are settled, final plan confirmation is pending.

## 1. Outcome and scope

Replace the Node-based CLI, controller, and agent with a native Rust implementation that is easier to
install and update. Deliver one `gridstator` executable for each supported OS/architecture, preserving
the workflows and capabilities of the current implementation. Keep a thin TypeScript VS Code
extension that uses VS Code's supplied runtime.

This is an implementation plan, not authorization to publish a release or change a running cluster.
The first stable v2 release requires complete parity and the evidence specified here. Development is
gated by working results, not a fixed date. Intermediate builds may expose an incomplete feature set
as explicitly marked development previews.

In this document, **v1** means the current Node-based generation, whose package version is currently
`0.1.0`; it does not imply that a `1.0.0` release exists. **v2** means the agreed native generation.

### Confirmed decisions

| ID | Decision |
| --- | --- |
| Q1–Q2 | Native executable, with no JavaScript runtime inside the standalone core. The goal is simpler installation and updates. Worker tools remain external. |
| Q3, Q6 | Linux/WSL2 x64 and ARM64; macOS Intel and Apple Silicon. All targets support CLI, controller, embedded worker, standalone worker, and automatic services. |
| Q4 | Preserve all current features, including managed updates and rollback. |
| Q5, Q9 | Rust for the CLI, worker, and controller, following the owner's explicit revision of the earlier Go choice. Maintained mainly by the owner with coding agents; prioritize straightforward maintenance. |
| Q7 | Fresh cluster installation is acceptable. Recreate credentials, rejoin workers, and archive old history separately. No old-state migration or mixed Node/native cluster support is required. |
| Q8 | Keep Node in VS Code's supplied extension runtime. Thin TypeScript extension: native CLI owns snapshots and local administration; extension owns editor UI and live API views. |
| Q10 | A Mac worker may wait for Docker to become ready, potentially after user login. |
| Q11 | The native scope and initial release gate were confirmed; the subsequent explicit Rust instruction supersedes the original language choice. |
| Q12 | Develop Rust alongside TypeScript in the same repository; keep v1 as a behavioral reference until cutover. |
| Q13 | Preserve CLI commands/flags, exit codes, project YAML, and JSON outputs. Explicitly document native update/runtime metadata exceptions. |
| Q14 | No fixed deadline. Release stable only after the acceptance gates pass. |
| Q15 | Provide a small verified installer and manual downloads. Retain explicit init/join/install-service commands and detect missing worker tools. |
| Q16 | Developer ID sign and notarize stable macOS releases. Credentials and the pipeline are release prerequisites; unsigned development builds are acceptable. |
| Q17 | Arrange suitable Intel and Apple Silicon Mac validation access. No Mac hardware is assumed available. |

### Deployment contract

- Release keys: `linux/amd64`, `linux/arm64`, `darwin/amd64`, `darwin/arm64`. Translate existing `x64`
  and Rust's `x86_64`/`aarch64` target naming explicitly; do not mix naming schemes in artifact lookup.
- Target machines do not need Node, npm, a Rust toolchain, a C compiler, or separately installed SQLite
  or OpenSSL libraries to run the standalone core. Normal OS libraries/facilities remain necessary.
  Build/test machines may need Rust, C/linker/SDK tooling, and Node for the extension/reference tests;
  jobs may use Node inside their containers.
- Workers still need Docker; image builds need buildx; Actions need `act` and git. Preserve managed
  `act` downloads. Remove management of the host Node runtime.
- Mac workers run the existing container-based job kinds. Native Mac support does not add native
  macOS/Xcode job execution. Windows remains supported through WSL2.
- Linux services use systemd; Mac services use launchd. Distinguish boot-start system services from
  login-start user services. Automatic startup does not imply Docker readiness or WSL availability.
- Minimum supported OS/kernel versions and the pinned Rust version must be declared in M1, based on
  the selected toolchain and dependencies. A target name alone is not an unlimited OS-version claim.

### Explicit exclusions

No legacy database/credential importer, transparent v1-to-v2 worker upgrade, mixed Node/native
production cluster, controller HA, native Windows service, host-process executor, new dashboard
design, or new execution backend. Current limitations remain unless a change is necessary for the
agreed platform support or is recorded as an explicit compatibility exception.

## 2. Establish the parity baseline

Planning reconnaissance inspected commit `bdc657ca30f70b3068530d4e397e114b6878bd4f` plus an active working
tree. That is not yet an immutable reference: existing local changes and ongoing v1 work must be
reconciled before M0 freezes the accepted baseline. Do not reset or discard that work.

[PLAN.md](PLAN.md) records the historical milestones and validation. Current source and tests include
newer update, toolchain, relocation, and extension behavior; use them to complete the inventory.
Historical test counts are not proof that v2 passes anything. No v2 implementation or validation has
been performed as part of writing this plan.

Create a parity ledger with one row per behavior, not merely one row per package. Each row records:
baseline source/test, expected observable behavior, native owner, acceptance case, implementation
status, and evidence location. Statuses are `not started`, `implemented`, `verified`, or an explicitly
approved exception. Unsupported tools or missing hardware leave a required row unverified.

| Behavior family | Reference implementation / tests | Required v2 evidence |
| --- | --- | --- |
| CLI and configuration | [CLI commands](../apps/cli/src/commands), [project config](../packages/protocol/src/project-config.ts), [README](../README.md) | Same commands, flags, defaults, YAML interpretation, exit codes, JSON shapes, job exit forwarding, and Ctrl-C cancel/detach behavior. |
| HTTPS, identity, and authorization | [PKI](../apps/controller/src/pki.ts), [auth](../apps/controller/src/auth.ts), [TLS client](../packages/shared/src/tls.ts), [security model](SECURITY.md) | CA creation/pinning, chain verification, token roles/expiry/revocation, join limits, ownership, and response/log secret redaction. |
| Jobs and reconciliation | [states](../packages/protocol/src/states.ts), [cluster](../apps/controller/src/cluster.ts), [controller tests](../apps/controller/src/controller.test.ts) | Legal transitions, assignment fencing, idempotency, cancellation races, retries, lost nodes, and controller/agent restart semantics. |
| Scheduling and host protection | [scheduler and tests](../packages/scheduler/src), [worker metrics](../apps/agent/src/system.ts) | Capability/label/node targeting, reservations, live-memory checks, concurrency, priorities/FIFO, backfill, rejection avoidance, affinity, preferences, shard spreading, and useful pending reasons. |
| Durable logs and events | [log pipeline](../apps/agent/src/log-pipeline.ts), [agent state](../apps/agent/src/state.ts), [controller logs](../apps/controller/src/logs.ts), [hub](../apps/controller/src/hub.ts) | Spool-before-send, UTF-8 correctness, masking/truncation, high-water-mark replay, result acknowledgment ordering, final partial lines, and subscription completion. |
| Working-tree snapshots | [manifest](../packages/protocol/src/manifest.ts), [capture and tests](../packages/snapshot/src), [client upload](../packages/snapshot/src/upload.ts) | Exact canonical IDs, tracked/untracked edits, submodules, ignore/include/secret precedence, executable bits, safe symlinks, stat-cache behavior, missing-only transfer, digest checks, and safe materialization. |
| Container commands | [container executor](../packages/executors/src/container.ts), [Docker wrapper](../packages/executors/src/docker.ts), [executor tests](../packages/executors/src) | Docker contexts/rootless access, resource limits, cache/workspace volumes, environment handling, live streams, exit/OOM, timeouts, graceful stop, and ownership-limited cleanup. |
| Docker builds | [build executor and tests](../packages/executors/src/build.ts) | Stages/args/secrets/platforms/cache, all output modes, metadata/labels, cancellation, path checks, artifact export, and cleanup. |
| GitHub Actions | [actions](../packages/executors/src/actions.ts), [checkout rewrite](../packages/executors/src/checkout-rewrite.ts), [act logs](../packages/executors/src/act-log.ts) | Events/inputs/matrix/git context, exact uncommitted checkout, local reusable workflows, secret handling, socket opt-in, unique ports, steps/artifacts, cancellation, and cleanup. |
| Discovery and reports | [discovery](../packages/discovery/src), [reports](../packages/reports/src) | Tasks/scripts/workflows/Docker stages, act caveats, JUnit variants, tar/zip artifact reports, shard attribution, and path mapping. |
| Groups and shards | [group model](../packages/protocol/src/group.ts), [group CLI](../apps/cli/src/commands/client.ts), [e2e](../tests/integration/e2e.test.ts) | Placeholder/env substitution, latest-attempt aggregation, state precedence, parallel placement, combined reports, cancel, and retry of non-successful shards only. |
| Persistence and retention | [database](../apps/controller/src/db.ts), [retention](../apps/controller/src/retention.ts), [CAS](../packages/snapshot/src/cas.ts) | Durable audit/history, fresh-v2 schema lifecycle, retained artifacts/snapshots/logs, safe mark-and-sweep, cache eviction, and private file permissions. |
| Services and local administration | [service commands](../apps/cli/src/commands/service.ts), [local controller](../apps/cli/src/local-controller.ts), [local tests](../apps/cli/src/local-controller.test.ts) | Role/data-directory ownership checks, install/replace/start/stop/restart/uninstall, shared local login, explicit revoked-access restoration, and stopped services remaining stopped. |
| Controller address relocation | [local configure](../apps/cli/src/local-controller.ts), [agent relocation](../apps/agent/src/relocation.ts), [relocation tests](../apps/agent/src/relocation.test.ts) | Verified listener/public-URL changes, aliases for offline workers, pinned-CA probe, persist-before-retarget, credential continuity, worker reconnection, and configuration restoration on failure. |
| Releases and toolchains | [updates](../apps/controller/src/updates.ts), [official releases](../apps/controller/src/releases.ts), [self-update](../apps/controller/src/self-update.ts), [install store](../packages/shared/src/install.ts), [toolchain](../apps/controller/src/toolchain.ts) | Stable/prerelease discovery and admin-only install, native asset filtering, controller mirror, drain/window/override policies, update-only recovery, health confirmation, crash/timeout rollback, database restoration, managed act, and actionable failures. |
| VS Code | [extension host](../apps/vscode/src), [webview](../apps/vscode/webview), [extension tests](../apps/vscode/test) | Workspace-host execution, shared login across windows, launcher/dashboard, unsaved-buffer checks, logs, Problems, Testing/JUnit, node controls, updates, and local administration. |

### Contracts that need focused fixtures

1. Capture defaults, limits, unknown-field policy, omitted versus null fields, empty objects/arrays,
   timestamp and byte units, fractional CPU values, error codes, redacted fields, and CLI stdout versus
   stderr behavior. Serde decoding must be followed by explicit validation/defaulting; Rust types and
   `Option` alone do not reproduce every schema distinction or bound.
2. Capture canonical snapshot bytes and IDs for ASCII, non-ASCII/non-BMP paths, HTML characters,
   control characters, links, and executable files. Rust JSON escaping and field order can differ from
   `JSON.stringify`; matching parsed objects is insufficient for content-addressed manifests.
3. Capture all legal job transitions, including assigned-to-running/terminal jumps after reconnect.
   Reports must match both node and assignment ID. Terminal jobs stay terminal; retries create new
   jobs with the correct lineage and retry budget.
4. Capture cancellation races, including success reported while cancellation is pending. Reproduce
   baseline behavior rather than imposing an apparently cleaner state machine without approval.
5. Capture log/result ordering: replay missing durable logs before terminal result acknowledgment.
   Network disconnects preserve jobs; agent process restarts clean owned resources and lose jobs.
6. Preserve per-file change detection and upload digest verification. Do not describe working-tree
   capture as an atomic whole-filesystem snapshot; that is stronger than the current implementation.

### Compatibility exceptions to enumerate

Keep ordinary workflow interfaces stable. Record changed fields/routes/flags in one versioned
exception list with replacement behavior and tests:

- Node runtime reporting and management disappear, including the Node toolchain setting and
  `agents set --node`. Worker placement flags such as `run --node` remain unchanged.
- Native build identity, target, artifact digest, release-manifest uploads, and updater metadata
  replace JavaScript entry-point identity and npm-tarball assumptions.
- Platform/service information expands for macOS and Docker VM capacity. Additive fields must not
  silently change the meaning of existing resource/configuration fields.
- Use explicit component/API/update-protocol compatibility checks. Preserve normal client job APIs
  where possible; version changed native update envelopes independently. Do not advertise v1/v2
  component interoperability merely because some endpoint shapes still match.

## 3. Architecture and implementation rules

### Repository organization

Start one Cargo package under `native/`, with `native/Cargo.toml`, a committed `Cargo.lock`,
`rust-toolchain.toml`, and `native/src/main.rs` as the single executable entry. Use internal library
modules for testable operations; introduce multiple workspace crates only when an actual boundary
justifies them.
Keep the TypeScript core, tests, and `apps/vscode` usable during development. Native test/build
commands must work independently of pnpm. Do not translate each TypeScript package into a Rust crate
by default.

Use internal boundaries around domain/protocol validation, controller state/persistence/scheduling,
worker/executors, workspace capture/CAS/materialization, clients/discovery/reports, installation and
supervision, platform services/metrics, and CLI presentation. Keep scheduling and canonical manifest
logic pure enough to exercise with fixtures. Packages expose complete operations rather than leaking
database handles, WebSocket details, or process supervision into callers.

The controller owns lifecycle and resource-allocation decisions behind one serialized coordination
boundary. Persist transitions atomically; perform slow network, filesystem, and Docker operations
outside that boundary and feed fenced results back. Agents report facts, spool logs/results, and
execute assigned work; they do not independently decide final job state.

Keep SQLite connections/transactions behind a dedicated owner thread or bounded blocking executor.
Bound hashing, archive processing, and blocking filesystem work so large snapshots cannot starve
heartbeats, cancellation, or scheduling. Do not hold a mutex guard across slow awaits. Explicitly
own assignment tasks and shutdown signals: stop new work, cancel or drain, wait for owned resource
cleanup, flush logs, and persist results before closing transport. Dropping a future is not a cleanup
protocol; started blocking work cannot simply be aborted ([Tokio documentation](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html)).

Define per-job ordering across async queues: durable spool append, send, controller append/high-water
update, terminal result, then acknowledgment. Bound queued bytes, keep slow subscribers from blocking
job control, and use a serialized/watermarked backlog-to-live subscription handoff with no lost or
duplicate log entries when completion races subscription.

### Dependency policy

Initial candidates are Tokio for async/process orchestration, an HTTP/WebSocket stack using rustls,
Serde/serde_json for wire types, clap for CLI parsing, and rusqlite with its bundled SQLite feature.
Also select certificate generation, maintained YAML parsing, hashes, tar/zip/XML, and platform metrics
crates. These are implementation candidates, not a commitment to every crate's default features.
[Bundled rusqlite](https://github.com/rusqlite/rusqlite#notes-on-building-rusqlite-and-libsqlite3-sys)
builds SQLite from source on the build machine; it must not create a target-machine SQLite install
requirement. Choose and test the [rustls crypto provider](https://docs.rs/rustls/latest/rustls/)
explicitly, including its native build requirements; avoid accidental dynamic OpenSSL dependencies.

M1 selects/pins versions and Cargo features, records license/maintenance suitability, and proves
these intended targets:

| Release key | Intended Rust target |
| --- | --- |
| `linux/amd64` | `x86_64-unknown-linux-musl` |
| `linux/arm64` | `aarch64-unknown-linux-musl` |
| `darwin/amd64` | `x86_64-apple-darwin` |
| `darwin/arm64` | `aarch64-apple-darwin` |

Use matching target C/linker tooling for bundled native dependencies and macOS SDK/build hosts for
Apple artifacts. Audit final binaries for dynamic dependencies: Linux releases should be standalone
static executables; macOS may link normal system libraries but must not require Homebrew-provided
SQLite/OpenSSL or another separately installed runtime. A successful `cargo build --release` alone
does not establish this property ([Rust linkage reference](https://doc.rust-lang.org/reference/linkage.html)).
Inspect dependencies and launch the final executable on each minimum supported native OS/architecture.

Keep contract fixtures language-neutral. Rust owns native validation/defaulting, with generated or
mechanically checked TypeScript API types for the thin extension. Choose the smallest workable schema
workflow in M1 and prove that invalid input, defaults, nullability, and error shapes remain aligned;
do not build a general protocol framework as part of this rewrite.

### macOS execution capacity and readiness

Separate process connectivity from execution readiness. Re-probe Docker/buildx/act/git after startup
and on relevant failures; advertise updated capabilities without requiring an agent restart. A
connected worker with unavailable Docker is unschedulable and explains why. Recover when Docker
starts or restarts, including Docker Desktop after login.

Model host pressure and Docker execution capacity separately. On macOS, Docker's Linux VM can be
much smaller than the Mac. Bound placement by the execution environment's CPU/RAM and reservations,
and retain host-protection signals. Do not use a daemon-reported Linux disk path with host `statfs`
and present that result as Docker storage availability. Unknown measurements remain visibly unknown.

Define reservation units and behavior explicitly in M1; prove them with a Docker VM deliberately
smaller than its host. Preserve Docker contexts and `DOCKER_HOST`, and distinguish host architecture
from the architecture of job images and requested build platforms.

### Native CLI and VS Code boundary

Keep the existing editor UI. Native commands own capture/upload, discovery needed for submission,
job-spec preparation, and local administration. The extension may use the live client API directly
for display/events and owns editor concerns such as dirty-buffer prompts, file locations, Problems,
Testing, notifications, and webview messaging.

Define versioned machine-readable helper requests/results and progress/cancellation behavior before
rewiring the extension. Keep JSON output clean; send diagnostics to stderr or explicitly typed
events. Pass secrets through private input/files rather than arguments. Remove the bundled JS
`local-cli.mjs` helper when native delegation is complete.

Resolve the executable on the workspace host using an explicit setting, the managed installation,
or PATH. Probe version/capabilities before invoking it. When missing/incompatible, explain the
required install/update action and offer an explicit installation path; do not silently execute a
binary supplied by workspace content. Test local, Remote SSH, and WSL windows, including concurrent
windows sharing the same account/login. Building the VSIX may still use Node.

## 4. Installation, releases, and update design

### Artifacts and shared release identity

The current updater distributes one architecture-independent JS bundle and compares every agent to
the controller's entry-point hash. Native binaries require a different model from the start:

- Allocate an immutable common release/build ID before compiling the four binaries and embed it
  with version/source/target metadata. Every new build invocation receives a new ID even when the
  displayed version is unchanged. Do not derive an embedded ID from a manifest containing the
  binary's own hash; that would create a circular dependency.
- Produce one immutable manifest for that ID containing provenance, protocol compatibility, and the
  four target entries. Hash the final manifest separately. Each entry records archive filename/size/
  SHA-256 and installed executable size/SHA-256 separately: archive verification alone does not
  establish which executable a worker is running. Calculate hashes after signing/final packaging.
- A worker is current when its release identity and the expected target artifact match. Its binary
  hash will usually differ from the controller's. Reject wrong-target, corrupt, missing, incomplete,
  or incompatible artifacts before activation.
- Official installation and local `publish-build` use this model. A local/offline release bundle
  contains a manifest and required target artifacts; missing targets produce an actionable error.
  Never compile missing worker binaries on a user's controller.
- Installer and manual-download paths acquire and verify the matching release manifest and record
  it in the managed store alongside executable identity. An unmanaged copied binary reports its
  embedded build ID/target and executable hash; managed publication requires the matching verified
  manifest/artifacts and explains how to obtain them. Do not assume the controller can reconstruct
  a complete native release from its own executable.
- Prove bootstrap from a fresh controller installed with only its target download: it obtains the
  immutable matching manifest and other required artifacts from the official source, verifies and
  mirrors them, then updates a different-target worker. Offline bootstrap accepts a complete local
  release bundle. An unavailable manifest/target disables that update with a useful reason.
- The controller mirrors verified artifacts and managed `act` archives so workers need no upstream
  internet for Gridstator/tool updates. This does not promise offline container pulls or Actions
  dependencies.
- Keep a small versioned update/bootstrap protocol that can recover a native agent unable to join
  the main agent channel. Freeze that protocol once released; future incompatible changes use new
  routes/envelopes.

### Native installation and supervision

Distribute one executable per target plus license/documentation/checksums. The installer detects
OS/architecture, downloads and verifies the selected artifact, installs to a suitable user or
explicit system path, and reports prerequisites. Keep manual download instructions as a fallback.
Do not automatically provision Docker or create a cluster as a side effect of downloading the CLI.

Use the distributed executable in a stable supervisor role for managed services, with versioned
payload directories and atomically activated state. The supervisor must start active native
executables without importing JavaScript or invoking a language runtime. Specify process ownership,
signal forwarding, concurrent install/update locking, fsync/rename ordering, confirmation deadlines,
crash-attempt accounting, and supervisor upgrade compatibility.

Preserve user/system installation, explicit data directories, permissions/ownership, force-replace,
uninstall with retained data, and useful service logs. launchd adapters must deliberately configure
the service account, HOME, PATH, Docker context/credentials, writable directories, and logs. Test paths
with spaces and non-default install locations.

### Managed update behavior

Preserve controller-orchestrated sequential worker rollout, scheduling holds, indefinite graceful
drain by default, explicit cancellation/deadline overrides, per-node policy/window overrides,
halt-on-failure, and explicit retry after a failed rollout. No background update may silently cancel
work because a drain took too long.

For controller self-update, hold new local assignments and drain its embedded worker before backing
up the database or activating the candidate. Explicit force/deadline options retain their documented
cancellation behavior. Start provisional confirmation deadlines when the candidate actually launches,
not when an arbitrarily long drain begins. A cancelled/failed pre-activation update restores prior
scheduling state without changing the active installation.

Activation is provisional. The supervisor rolls back on repeated crashes or missed health
confirmation. Worker confirmation requires a healthy native process and compatible authenticated
controller handshake/reconciliation, independently of temporary Docker readiness. Controller
confirmation requires working persistence/migrations and the API/coordinator serving normally.

Back up controller SQLite consistently, including WAL state, before switching a version that may
migrate it. Restore the matching database backup before the old controller opens it after failed
startup. A binary-only rollback is insufficient. This recovery contract covers failed provisional
updates; it does not promise arbitrary downgrade after a healthy version has accepted new writes.
Retain the failed database for diagnosis, with private permissions.

Test updates from native build A to B, including builds sharing a display version, on a mixed
Linux/macOS and x64/ARM64 native cluster. Legacy npm packages are archived for v1 recovery; native
rollout does not install them or convert old Node services.

Preserve official release discovery/install: stable releases by default, prerelease opt-in, release
notes/version feedback, native-manifest-compatible asset filtering, and admin-only installation.
Exclude legacy npm `.tgz` releases from native install choices. Reject missing/ambiguous/mismatched
checksum metadata, and leave the active installation unchanged on discovery/download failure. Cover
this both through CLI/API tests and the extension Settings flow.

### macOS distribution

Developer ID signing and notarization are stable-release gates. Arrange the Apple account,
certificates, notarization credentials, and protected CI signing environment before the release
stage. Validate the delivered download on clean Macs through the actual documented acquisition
path. Do not infer this result from a locally built executable that was never quarantined.

Use Apple's supported signing/notarization workflow and verify the final package/signature. Publish
unsigned development builds only as explicitly identified previews. This follows
[Apple's Developer ID guidance](https://developer.apple.com/developer-id/) and
[notarization documentation](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution).

## 5. Implementation milestones

Each milestone produces usable behavior, tests, and a parity-ledger update. Do not close a milestone
because its files were ported. The feature sequence below is recommended; refine work-package size
without weakening the exit gates. Release engineering and Mac access begin early.

| Milestone | Depends on | Working result / exit gate |
| --- | --- | --- |
| M0: freeze contracts and evidence | — | Accepted v1 baseline, command/API/config inventory, compatibility exceptions, fixtures, parity ledger, validation-host plan, and explicit v1/v2 data/service separation. |
| M1: prove native/platform foundations | M0 | One Cargo package; pinned dependencies/features/toolchain and OS baselines; all four target builds/linkage audits; native TLS/SQLite/snapshot/async probes; supervisor rollback proof; launchd/readiness/VM-capacity proof. |
| M2: first native remote command | M1a/M1b for the target being exercised | Packaged Rust CLI initializes a controller, joins a separate agent, runs a real Docker command, streams logs, returns its exit code, cancels it, and cleans owned resources. Mac behavior also requires M1c. |
| M3: exact workspace execution | M2 | Native snapshots, missing-only transfer/CAS, cache, safe staging, and CLI submission execute uncommitted files correctly; canonical and malicious-input fixtures pass. |
| M4: durable lifecycle and full scheduling | M2, M3 | Restart/reconnect/log replay, assignment fencing, cancellation races, retry lineage, retention, full placement/resource behavior, and host-protection tests pass. |
| M5: builds and Actions | M3 | Native build/actions executors preserve current options, checkout protection, outputs, artifacts, secrets, socket behavior, timeout/cancel, and cleanup with real tools. |
| M6: complete workflow surface | M4, M5 | Groups/shards, JUnit/reports, discovery, doctor/status/node/token/config commands, preferences, and documented CLI/JSON parity are verified. |
| M7: production installation and updates | M1, M4, M5 | systemd/launchd adapters, local admin/endpoint relocation, installer, managed act, per-target rollout, controller self-update, and executable/database rollback pass actual service tests. |
| M8: thin VS Code integration | M3, M6, native helper contract from M7 | Packaged VSIX delegates to the native CLI on the workspace host, preserves current UI/feedback/login behavior, and handles missing/incompatible binaries. |
| M9: release candidate and v1 cutover | M0–M8 | Complete parity ledger, four-target runtime/service evidence, mixed-target updates, signed/notarized Mac artifacts, documented fresh installation and recovery rehearsal, release review. |

### M0 — baseline before translation

- Record the exact accepted source revision and disposition of intentional in-progress changes.
  Capture help/JSON/config examples and language-neutral golden fixtures. Normalize only genuinely
  variable fields, such as generated IDs/time; keep behavior-sensitive order/defaults unchanged.
- Inventory current source/tests beyond historical docs: releases, managed act, shared/local login,
  controller listening-address relocation, update windows, and current extension behavior.
- Create the compatibility exception list and a machine-readable parity ledger. Record known v1
  limitations separately from v2 defects.
- Establish isolated v2 data/config/service names during development. Never open a v1 database or
  alter its service through an ambiguous default path.
- Arrange validation access and Apple release credentials as owned prerequisite tasks, with the
  project owner supplying human-only account/access actions. No dates or access are assumed.

### M1 — retire the hardest deployment risks early

- **M1a: dependency and protocol proof.** Build all four Rust targets with the selected native build
  toolchains and audit their linkage; exercise TLS pinning/certificate creation, SQLite transactions/
  backups, WebSockets, YAML/default validation, canonical manifest bytes, and process signals on
  native hosts. Prove large blocking/hash/archive work does not stall heartbeats or cancellation.
  Pin minimum OS versions, Rust, dependencies, and features.
- **M1b: supervisor proof.** Define release/artifact identity and version-store state, then demonstrate
  successful provisional activation, corrupt/wrong-target rejection, repeated-crash rollback,
  unconfirmed/hung-startup rollback, and atomic recovery after interruption. A small controlled child
  process is sufficient here; real controller/worker upgrade evidence is still required in M7.
- **M1c: Mac proof.** Install a minimal launchd service, verify its user/environment/path, discover
  Docker Desktop after late startup, survive Desktop restart, measure execution VM limits separately
  from host pressure, and confirm architecture/image handling. Exercise both Mac architectures.
- If a proof fails, record the required design change and re-run it before widening the dependent
  implementation. Missing host access leaves that target's proof open; it is not replaced by a
  cross-build success. Independent Linux work may continue while Mac access is arranged. All four
  targets' proofs remain mandatory before M9 and before claiming those targets work.

### M2–M4 — establish a reliable distributed core

- Use real HTTPS/WSS, SQLite, and Docker for the first end-to-end slice. Start with a minimal
  unsnapshotted command, then add workspace capture rather than mocking the whole execution path.
- Port pure state/scheduler/manifest fixtures before relying on their behavior in the cluster.
  Keep lifecycle decisions serialized and allocation accounting correct within a scheduling pass.
- Preserve both staging modes: tar into Docker volumes for commands; safe host materialization for
  builds/actions. Retain backpressure, digest verification, ownership labels, and cleanup boundaries.
- Demonstrate controller restart during a job, network disconnect through completion, stale and
  duplicate reports, worker process death, rejected assignments, cancellation timeout/races, automatic
  retries, and log replay above the durable high-water mark.
- Verify explicit assignment-task shutdown, backlog-to-live log handoff, and result publication only
  after output pipes/spools are drained. Docker cancellation must stop the container, not merely
  kill its attached CLI; buildx/act retain signal/grace/escalation behavior on Linux and macOS.
- Exercise authorization/redaction and archive/path safety throughout these slices. These are
  existing contracts and cannot be deferred until packaging.

### M5–M6 — close feature parity

- Run real BuildKit and act acceptance scenarios, including image tar via load/save, explicit
  same-repository checkout refs, local reusable workflows, secret-safe arguments/files, multiline
  values, artifact collection, and socket-enabled nested Docker workflows.
- Preserve Docker contexts, cache persistence, ownership repair, runner image/platform selection,
  and current resource-limit limitations. Build resource requests remain scheduling reservations
  where the current build executor cannot enforce cgroup limits.
- Verify group aggregation uses latest attempts, state precedence waits for all shards, one-worker
  scheduling still works, and group retry does not rerun successful shards.
- Test representative JUnit formats and tar/zip report artifacts, workflow/script/Docker discovery,
  problem path mapping inputs, CLI errors, JSON output, and documented configuration precedence.

### M7–M8 — operational parity and editor integration

- Replace unit rendering-only confidence with actual install/start/stop/restart/uninstall and
  upgrade/rollback exercises under systemd and launchd. Preserve local account ownership checks,
  revoked-login restoration, and intentionally stopped services.
- Verify controller endpoint relocation with online and temporarily offline workers, persistent
  fallback endpoints, pinned-CA checks, unchanged credentials, and restoration after failed changes.
  This is endpoint relocation, not controller data migration between hosts.
- Retain managed act version/pin/override/off policy and per-target mirrored archives. Native v2
  must not request/install Node to manage itself.
- Complete native helper JSON/progress/error contracts, then rewire the extension. Reuse dashboard
  UI tests and real VS Code integration scenarios against native services, including Remote SSH/WSL.
- Test concurrent windows, cancellation during capture/submission, paths containing spaces, missing
  CLI, incompatible versions, and shared login changes without requiring an editor reload.

### Parallel work boundaries

After M0 fixes shared contracts, independent work can cover platform/supervisor proofs, pure
scheduler/manifest fixtures, and controller foundations. After M3, M4 and M5 can proceed in parallel
behind stable executor/protocol boundaries. Extension adapter work can begin when helper contracts
stabilize; UI replacement is not part of its scope. One implementer owns each shared contract change,
and integration happens through working cluster slices rather than divergent private APIs.

Every work package must identify prerequisites, owned source area, reference behavior, tests,
documentation/exception impact, and proof of completion. Coding agents may implement bounded packages;
the project owner owns hardware/account access and the final release decision. Do not make the
owner answer routine library or package-layout choices already assigned to M1.

## 6. Validation and release evidence

Create native unit/fixture suites, black-box process/cluster tests, real Docker/buildx/act tests, OS
service tests, and packaged VSIX tests. Reuse v1 scenarios as behavioral specifications. The v1 Node
harness may temporarily drive native executables in CI, but target-runtime-free proof must use a
separate clean environment where Node/npm and language toolchains are unavailable.

Run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and unit/integration tests with
the pinned production feature set. Audit release linkage and test Cargo feature combinations that
affect transport/storage/builds. Add targeted concurrency stress or model tests where they can expose
ordering/shutdown errors; ownership checking does not prove distributed behavior. Keep TypeScript
checks for the extension and retained reference code. Required real-tool tests fail when their
prerequisite is missing instead of silently skipping and reporting a successful release gate.

### Required evidence matrix

| Evidence | Linux x64 | Linux ARM64 | Mac Intel | Mac Apple Silicon |
| --- | --- | --- | --- | --- |
| Native release build and target/version metadata | Required | Required | Required | Required |
| Clean target installation without Node/npm/Rust-toolchain/SQLite/OpenSSL installation | Required | Required | Required | Required |
| CLI, controller, embedded worker, standalone worker | Required | Required | Required | Required |
| Snapshot + command/build/Actions + logs/artifacts/groups/reports | Required | Required | Required | Required |
| Actual service lifecycle, restart/logout/reboot as applicable | Required | Required | Required | Required |
| Late Docker startup/restart and correct resource accounting | Required | Required | Required | Required |
| Successful native update and crash/timeout rollback | Required | Required | Required | Required |
| Final download/distribution verification | Required | Required | Signed/notarized | Signed/notarized |

Also require a WSL2 operational run on Windows, a real LAN run spanning at least two machines,
mixed-target native rollout, controller schema-change rollback, and real VS Code local/Remote SSH
workflow evidence. A Linux binary test is not a WSL boot/networking test. Do not claim a Windows boot
service guarantee from systemd enablement inside a distribution that is not running.

Use hosted runners for native build/unit/smoke work where appropriate. Arrange Docker-capable Macs
for Mac executor, service, reboot, and update tests. GitHub currently documents that hosted Apple
Silicon macOS runners do not support nested virtualization; do not assume a Docker Desktop test can
run there ([runner reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners#limitations-for-arm64-macos-runners)).
Docker Desktop's documented automatic startup is at sign-in, so readiness and automatic service
startup must be checked separately ([Docker settings](https://docs.docker.com/desktop/settings-and-maintenance/settings/)).

### Failure-injection suite

- Disconnect a worker while it runs; restore connectivity after completion; verify all expected log
  sequences and exactly one terminal outcome. Restart the controller without losing running jobs.
- Kill the agent; restart it; verify only its owned containers/volumes are cleaned and lost/retry
  behavior matches the baseline. Inject stale assignment reports and duplicate results.
- Attempt corrupt/missing/wrong-target release installation; interrupt download/staging/activation;
  start crashing and hung/unconfirmed versions; verify deterministic recovery and actionable status.
- Apply a controller schema-changing candidate that fails startup; verify the old executable opens
  the restored pre-update database and the failed copy remains available for diagnosis.
- Run long jobs through a default update request; verify indefinite drain, no silent cancellation,
  explicit force/deadline behavior, and rollout halt after a worker failure.
- Keep an embedded worker busy longer than the candidate's normal confirmation timeout; verify
  controller update drains first, starts the deadline at launch, and restores scheduling if aborted.
- Remove upstream access from workers while leaving controller connectivity; verify Gridstator/act
  updates use the controller mirror. Keep this separate from job-image/Actions network requirements.
- Start Mac services before Docker, then start/restart Docker; verify connected/degraded versus
  schedulable states, recovery without agent restart, and placement bounded by smaller VM limits.

### Completion records

For each gate, retain source revision, artifact/manifest hashes, target OS/architecture/version,
tool versions, exact command or scenario, result, logs, and relevant screenshots/VS Code output.
Record unavailable access or skipped tests as missing evidence. The release job consumes artifacts
from the tested candidate; do not silently rebuild different bytes after validation. Signing is
part of producing the candidate bytes whose checksums and distribution path are verified.

Stable release requires every required parity row and matrix cell verified, compatibility exceptions
documented, installer/manual instructions rehearsed, and the owner reviewing the evidence. No date
or partially passing platform matrix overrides this gate.

## 7. Fresh-cluster cutover and recovery runbook

1. Finish the parity/release gates using isolated v2 state and services. Before production cutover,
   record v1 locations, service definitions, CLI/VSIX versions, and recovery commands.
2. Drain v1 and finish active work. Stop its services. Make consistent archives of its controller
   database/WAL state, configuration, CA/private key, credentials, agent state, logs, blobs, and any
   installation artifacts needed to restore it. Protect the archive as credential-bearing data.
3. Install the verified v2 binary. Initialize a new cluster in a new data directory, create new
   credentials, and join workers. Reuse project `gridstator.yml` files under the preserved workflow
   contract; do not point v2 at v1 state or leave conflicting listeners/services active.
4. Install v2 services and the matching VSIX; sign in on development/workspace hosts. Check nodes,
   capacity/reservations, executor readiness, shared login, and local service ownership.
5. Run representative command, build, Actions, artifact/JUnit, shard, cancellation/retry, and
   reconnect scenarios. Exercise a native update/rollback on a non-critical worker before relying on
   the new cluster for normal work.
6. Keep v1 history archived/read-only. No merged history or preserved v1 job IDs/tokens are promised.
   Set archive retention explicitly; this plan does not authorize deletion of old cluster data.
7. If the cutover fails, stop v2 before restoring the archived v1 services/configuration and old
   extension/login. This restores the separate old cluster; it does not merge jobs created in v2
   into v1. Rehearse the runbook before the owner chooses the live cutover window.

At stable cutover, make native instructions/artifacts the default, stop publishing the Node CLI as
the current generation, and retain a tagged v1 reference/recovery path. Remove redundant TypeScript
core/build plumbing only after the baseline and recovery artifacts are preserved and native evidence
is complete. Retain the TypeScript extension and its build tooling.

## 8. Risks, ownership, and exit policy

| Risk | Mitigation / owner |
| --- | --- |
| Scope expands while v1 continues changing | Freeze an accepted baseline in M0; review later v1 changes explicitly against the parity ledger. Implementer maintains the ledger. |
| Rust/Serde produces subtly different manifests/defaults/JSON | Byte-level and semantic fixtures, explicit validation/defaulting, and compatibility exceptions. Implementer owns fixes. |
| Async ownership/shutdown changes lifecycle or loses logs | Serialized decisions, fenced results, bounded blocking/queues, explicit task shutdown, durable sequence/ack ordering, and failure-injection scenarios. |
| Mac VM capacity differs from host resources | Early M1 probe, explicit capacity model, smaller-VM acceptance scenario, readiness re-probes. |
| Native update selects wrong artifact or cannot recover | Shared release identity plus target digests; early supervisor proof; actual mixed-target service and database rollback tests. |
| Mac tests/signing cannot run | Owner arranges Intel/Apple Silicon validation access and Apple release credentials early. Stable release remains gated until evidence exists. |
| Thin extension introduces binary/version friction | Workspace-host binary discovery, explicit installation guidance, capability handshake, and packaged VSIX tests. |
| v1 and v2 touch the same live data/service | Isolated development paths, fresh credentials, explicit cutover/archive/recovery rehearsal. |
| Passing CI conceals missing real-tool/platform coverage | Required jobs fail on missing prerequisites; evidence matrix tracks actual hosts/services and final artifact hashes. |

The next implementation step is M0, followed by the three bounded M1 proofs. Do not start by deleting
the TypeScript core or translating every package. Reassess technical choices at proof failures;
reopen settled product decisions only when new evidence makes the agreed requirements incompatible.

## 9. Plan confirmation

Scope decisions Q1–Q17 are recorded above. Q18 confirms this concrete plan and its release gates.
Software implementation, publishing, and changing a live cluster are separate tasks.
