# Gridstator: Plan and Progress

Last updated: 2026-11-09. **Current state: all seven milestones are implemented and validated,
including across two physical machines on the LAN (Linux laptop controller + WSL2 worker on a
Windows PC), and installed there as services. Next: tag the first release (v0.1.0) when the owner
is ready, then iterate on feedback.**

The native Rust generation is planned separately in [V2_UPGRADE_PLAN.md](V2_UPGRADE_PLAN.md). The
milestones and validation history below describe the current Node-based implementation.

## Milestones

| # | Milestone | Status |
|---|---|---|
| 1 | Distributed execution core: controller, agents, CLI, protocol, container executor | ✅ Implemented; validated locally and across two machines |
| 2 | Workspace snapshots: capture, content-addressed transfer, staging, cleanup | ✅ Implemented; validated locally and across two machines |
| 3 | Docker image builds with BuildKit/buildx | ✅ Implemented; validated locally and across two machines |
| 4 | Local GitHub Actions via `act` | ✅ Implemented; validated locally and across two machines |
| 5 | VS Code extension and dashboard | ✅ Implemented; validated in headless VS Code (local and against the LAN cluster); Remote SSH UI walkthrough by the owner pending |
| 6 | Advanced scheduling, test sharding, artifacts, retry policies | ✅ Implemented; validated locally and across two machines |
| 7 | Open-source release readiness | ✅ Implemented; install path validated on both machines; release workflow ready (no tag pushed yet) |

## Milestone 1: distributed execution core ✅

Delivered:

- [x] pnpm monorepo, strict TypeScript project references, Vitest, ESLint (type-aware), Prettier, CI workflow
- [x] Shared protocol (zod): job spec, state machine, nodes, REST bodies, agent and client WebSocket messages
- [x] Controller: Fastify over TLS, SQLite (`node:sqlite`) persistence, CA + server certificate generation
- [x] Tokens: named/revocable client tokens (admin/operator), expiring limited-use join tokens, per-node agent credentials
- [x] Agent registration (join), CA fingerprint pinning, node info and live metrics, heartbeats
- [x] Pure capability/label/resource-aware scheduler with reservations, concurrency limits, priorities, backfill, warm-cache affinity, pending reasons
- [x] Container executor (docker CLI): labels, limits, `--init`, `no-new-privileges`, env-file secrets, timeout, cancel, cleanup
- [x] Log pipeline: UTF-8-safe line buffering, secret masking, batching, size cap, spool + replay
- [x] Cancel (queued and running, with timeout fallback), manual retry, automatic retry on `lost`/`failed`
- [x] Lost-node detection, assignment timeout, rejection handling, controller-restart and agent-restart reconciliation
- [x] Persistent job history with audit events; retention sweeps for jobs, logs, snapshots, blobs
- [x] CLI: `controller init/start [--with-agent]`, `agent join/start`, `login`, `status`, `doctor`, `nodes …`, `jobs`, `job`, `run`, `logs [-f]`, `cancel`, `retry`, `token …`, `init`, `snapshot`
- [x] Single-file bundle and npm tarball (`pnpm pack:release`), verified by installing into a clean prefix

Acceptance criterion: *a command submitted from the development laptop runs on a different machine,
logs stream back in real time, and the job can be cancelled.* **Validated across two machines**
(see the two-machine log below) and with separate agent processes on one host (automated e2e).

## Milestone 2: workspace snapshots ✅

Delivered:

- [x] Project root discovery (`gridstator.yml` → git top-level → cwd) and relative workdir
- [x] File selection via `git ls-files --cached --others --exclude-standard` (submodules recursed); `.gitignore`-aware walker without git
- [x] Built-in excludes (`.git`, `node_modules`), secret patterns excluded even when tracked (with warnings), `include`/`exclude`/`allowSecrets`, max file size
- [x] Content-addressed manifest; snapshot ID = SHA-256 of the canonical manifest; stat-based hash cache; files that change mid-capture are re-read
- [x] Upload of missing blobs only (tar batches), digest verification on the controller, snapshot registration with completeness check
- [x] Agent blob cache with LRU eviction; workspace streamed into a per-job Docker volume (no host bind mounts)
- [x] Hardened host-directory materializer (for `act` in M4) with path, symlink and `O_EXCL|O_NOFOLLOW` protections
- [x] `gridstator snapshot [--dry-run] [--json]` to inspect exactly what is sent

Acceptance criterion: *change a test locally without committing; execute remotely; the worker runs
the changed version.* Validated across two machines and in the automated e2e test
`runs the exact uncommitted working tree`.

## Milestone 3: Docker builds ✅

- [x] `docker-build` job kind and executor: snapshot staged with the hardened materializer, Dockerfile/context resolved inside it, `--target`, build args, build secrets (`--secret env=`), tags, platforms, `--no-cache`, `--pull`, external cache import/export
- [x] Outputs `load`, `push`, `tar` (via `docker save`, uploaded as `image.tar`), `none`; image ID, digest, tags and size reported as job outputs; images labelled with the job
- [x] Live BuildKit progress; cancellation (SIGINT, then SIGKILL); timeout; staging cleanup
- [x] Artifact infrastructure: agents upload to the controller's CAS; `artifacts` table; list/download API and `gridstator artifacts`
- [x] `gridstator build` (and named `builds:` in gridstator.yml); `examples/docker-build`

Acceptance: *build a Next.js image on a different machine while development continues on the laptop;
logs and result visible through the controller.* **Met** — see the M3–M5 two-machine log below.

## Milestone 4: GitHub Actions via act ✅

- [x] Workflow discovery with jobs, events, `workflow_dispatch` inputs and caveat detection (`gridstator actions list`)
- [x] `actions` job kind and executor: staged snapshot with a git context mirroring the developer's branch and remote; whole workflow or one job; event, inputs, vars, env, secrets, runner image mapping, matrix filter, optional event payload
- [x] Hardened act defaults (ADR-15): no Docker socket unless requested, private secret/var/env files, loopback cache server, per-job artifact port, labelled containers and crash cleanup
- [x] act JSON logs rendered in act's console format; structured steps published live; workflow artifacts captured
- [x] `gridstator actions run`; `examples/github-actions`

Acceptance: *run an existing workflow on a remote worker using uncommitted local changes, without
pushing.* **Met** — see below.

## Milestone 5: VS Code extension ✅

- [x] Activity Bar container with Cluster, Jobs and Run views (welcome content when signed out or empty)
- [x] React dashboard webview: overview, jobs (filters, search, row actions), job details (live virtualized logs with search, stream filters, follow, wrap, clickable file paths; steps; image; artifacts with download; history; cancel, retry, rerun with current files), run launcher (tasks, builds, workflows with job/event/inputs, scripts, Dockerfiles with stage/tags/output, custom command, worker and resources), node management (scheduling mode, concurrency, reservations, labels)
- [x] Commands (run command/task/script/workflow/build, rerun last, select target worker, sign in, refresh, clear problems), status bar item, finish notifications with actions
- [x] Unsaved-editor check before snapshots (`gridstator.unsavedFiles`)
- [x] Problems panel from job logs (tsc, file:line:col tools, ESLint stylish, test stack traces) and Testing view from JUnit artifacts, plus a *Run on Gridstator* test profile
- [x] Works on the workspace host (Remote SSH) and shares the CLI's connection; VSIX packaging

Acceptance: *open a project through Remote SSH, run tests on the 64 GB worker, see the job in the
dashboard, stream logs, cancel or retry from VS Code.* Everything except the Remote SSH UI walkthrough
was validated automatically in a real (headless) VS Code against the LAN cluster with big-box as the
target; the extension runs on the workspace host exactly as it does under Remote SSH.

## Milestone 6: scheduling and parallel testing ✅

- [x] Capability-aware scheduling, resource reservations, priorities, concurrency limits, drain, retry policies, artifact collection, worker failure recovery (from M1–M5)
- [x] **Preferred workers**: `target.prefer` (soft) — `--prefer`, `defaults.prefer`/task `prefer` in gridstator.yml, and "prefer, don't require" in VS Code
- [x] **Job groups and test sharding**: `POST /v1/groups`, `{shard}`/`{total}` substitution and `GRIDSTATOR_SHARD_*`, aggregate group state, cancel, retry of failed shards only, `group.updated` events, schema v3
- [x] Scheduler spreads shards of a group across nodes (score term, ADR-20)
- [x] Combined JUnit results across shards (`packages/reports`): `gridstator group show`, the dashboard's group view (timeline, shards, combined tests) and one combined Testing-view run in VS Code
- [x] CLI: `run --shards`, `groups`, `group show|cancel|retry`, shard-prefixed live logs; `examples/sharded-tests`

Acceptance: *two independent test shards execute simultaneously on different workers and their
combined results are visible in a single job group.* **Met** — automated e2e (`alpha`/`beta`) and on
the real cluster (below).

## Milestone 7: open-source release readiness ✅

- [x] Linux installation and bootstrap guide ([INSTALL.md](INSTALL.md)); `controller install-service` / `agent install-service [--system]` / `uninstall-service`
- [x] Windows/WSL2 guide ([WINDOWS.md](WINDOWS.md)), validated with the big-box worker
- [x] Packaging: single-file npm package (no runtime dependencies) and VSIX; version stamping (`scripts/set-version.mjs`)
- [x] Release automation (`.github/workflows/release.yml`): checks, GitHub release with tarball, VSIX and `SHA256SUMS`, notes from CHANGELOG; npm (with provenance) and Marketplace publishing when `NPM_TOKEN` / `VSCE_PAT` are set
- [x] Troubleshooting guide ([TROUBLESHOOTING.md](TROUBLESHOOTING.md)), CHANGELOG, Code of Conduct (Contributor Covenant 2.1), security policy, issue and PR templates, Dependabot
- [x] Example projects: node-project, docker-build, github-actions, sharded-tests
- [x] Architecture, security, API and development docs updated; integration tests in CI (Docker, BuildKit, act, VS Code)

## Validation log (2026-11-09, Linux laptop, Node 24.15, Docker 29.0.2)

| Check | Result |
|---|---|
| `pnpm typecheck` (sources + tests) | pass |
| `pnpm lint`, `pnpm format:check` | pass |
| `pnpm test`: 50 tests (scheduler 14, snapshot 14, executor 6 with real Docker, controller protocol 16) | pass |
| `pnpm test:e2e`: 9 tests, built CLI, controller + 2 agents, real Docker | pass |
| Packed tarball installed into a clean npm prefix; init → start → snapshot run | pass |
| Manual: controller restart mid-job (15/15 log lines, job succeeded) | pass |
| Manual: `kill -9` agent mid-job → orphan container removed on restart, job `lost`, auto-retry succeeded | pass |
| Node 22 | not run locally; **passes in CI** (GitHub Actions, first run on 2026-11-09) |
| GitHub Actions CI: Node 22 + 24 checks, end-to-end with Docker, bundle + smoke test | pass |

## Two-machine validation (2026-11-09)

Controller + embedded agent `laptop` (reserve 4 CPU / 16 GiB) on the Linux laptop
(8 CPU). Worker `big-box`: Ubuntu 24.04 under **WSL2** on a Windows PC
(16 CPU, 30 GiB visible to WSL), Docker Engine 29.9.3 + buildx installed from
Docker's apt repository, Node 24.22.2 from the checksum-verified nodejs.org tarball, Gridstator
from the packed tarball. Agent run as a systemd service. Procedure: [DEVELOPMENT.md](DEVELOPMENT.md#manual-multi-machine-validation).

| Step | Result |
|---|---|
| WSL2 agent dials out to the laptop controller (no inbound ports on the worker) | pass |
| Job submitted on the laptop without `--node` is placed on `big-box` automatically; `nproc` in the job = 16 | pass |
| Live streaming: lines received on the laptop 1 s apart as printed; remote `docker pull` progress streamed | pass |
| Cancel a running remote job: `cancelled` in 0.94 s; no container or volume left on `big-box` | pass |
| Retry: attempt 2 with `retryOf` lineage and audit history | pass |
| M2: uncommitted test edit runs on `big-box` (fails as expected); agent fetched only 1 of 5 blobs | pass |
| Controller restarted for ~9 s mid-job: worker reconnected (`resumed: 1`), 15/15 lines, job `succeeded` | pass |
| `SIGKILL` of the worker agent mid-job: on restart it removed the orphan container/volume; job `lost`; automatic retry succeeded on attempt 2 | pass |

Findings from this run: a multi-line system log message (Docker pull output) was only prefixed on
its first line in the CLI (fixed). A user newly added to the `docker` group must log in again
before an agent started from that session can use Docker; the agent reports this clearly as
"Docker unavailable" and the node shows as degraded.

## Validation log: Milestones 3–5 (2026-11-09)

| Check | Result |
|---|---|
| `pnpm typecheck`, `pnpm lint`, `pnpm format:check` (all packages, extension, webview, tests) | pass |
| `pnpm test`: 76 tests incl. real BuildKit builds (stages, secrets, export, cancel) and real act runs (checkout skip, git context, inputs, secrets masked, steps, artifacts, failure, cancel + cleanup) | pass |
| `pnpm test:e2e`: 11 tests incl. remote Docker build and act workflow with artifact download via the CLI | pass |
| `pnpm test:ui`: 7 Playwright tests of the dashboard (all views and actions) | pass |
| `pnpm test:vscode`: extension in real VS Code 1.145.2 (headless) against a local cluster | pass |
| VSIX packaged (385 KB) | pass |

**Two machines (laptop controller + big-box WSL2 worker, act 0.2.97 and the new build installed on big-box):**

| Step | Result |
|---|---|
| M3: `create-next-app` project (Next.js 16.4, standalone output, multi-stage Dockerfile) with an uncommitted page edit, `gridstator build --node big-box --target runner` | built in 30 s with live BuildKit logs; image labelled with the job; running it on big-box serves the uncommitted text; rebuild 1.1 s (warm cache on big-box) |
| M4: `examples/github-actions` with uncommitted source and test edits, `gridstator actions run ci.yml --node big-box -e workflow_dispatch -i greeting=LAN` | test → lint in `needs` order, passing with the uncommitted code; report artifact shows `Hello LAN from example-org/demo-app@feature/demo` |
| M5: VS Code integration test with `GRIDSTATOR_IT_CONFIG` = LAN cluster, target `big-box` | task ran on big-box (16 CPU); 3 JUnit results in the Testing view; failing assertion in the Problems panel at the right line; cancel, retry, drain/enable from VS Code commands |

Findings fixed during M3–M5: buildx cannot export a tar with the default `docker` driver (now
`docker save`); act binds its cache server to the LAN address and uses a fixed artifact port (now
loopback and per-job); act timestamps have 1 s resolution (steps now use receive time); Node's JUnit
reporter emits test cases directly under `<testsuites>`; two accessibility defects in the dashboard
(buttons named by tooltip instead of text; select names polluted by option text).

## Validation log: Milestones 6–7 (2026-11-09)

| Check | Result |
|---|---|
| Static checks (typecheck, lint, format) | pass |
| `pnpm test`: 84 tests (adds group/prefer scheduler tests, group controller tests, reports, systemd units) | pass |
| `pnpm test:e2e`: 12 tests incl. 2 shards on `alpha` and `beta` in parallel with 8/8 combined JUnit results | pass |
| `pnpm test:ui`: 9 tests incl. group view and sharded launch | pass |
| `pnpm test:vscode`: adds a 2-shard launch whose results are combined into one Testing run | pass |
| GitHub Actions CI on `main` (Node 22/24, e2e with act, VS Code jobs, packaging) | pass |

**Real cluster (installed services):**

| Step | Result |
|---|---|
| Upgrade the laptop: `npm i -g` new tarball, `gridstator controller install-service --with-agent --agent-name laptop` | user service running; database migrated to v3 with node settings intact; lingering enabled |
| Upgrade big-box: `sudo gridstator agent install-service --system --force` | system unit `User=pj`, `SupplementaryGroups=docker`, explicit data dir; agent reconnected |
| `gridstator run --task test` in `examples/sharded-tests` | shard 1 on big-box, shard 2 on the laptop, overlapping 6.5 s; group succeeded in 6.8 s (12 s serially); 8/8 tests combined |

Finding fixed: `install-service --force` did not restart an already running unit, so the old
process kept running; it now restarts the service.

## Decisions

Settled with the project owner (2026-11-09):

| # | Topic | Decision |
|---|---|---|
| D1 | Controller placement | One binary; controller and agent can run in one process on any node |
| D2 | Snapshot transfer | Content-addressed store on the controller; agents pull and cache |
| D3 | Transport security | Self-generated CA, fingerprint pinning, bearer tokens |
| D4 | Session scope | Docs + M1 + M2, then review |
| D5 | Execution | Containers only; host execution later as an explicit opt-in |
| D6 | Dev laptop | Normal worker with reserved CPU/RAM |
| D7 | Packaging | Single npm package `gridstator` |
| D8 | License | Apache-2.0 |
| D9 | Dependencies | Persistent cache volume; the job runs the install |
| D10 | Project config | Optional `gridstator.yml` |
| D11 | File set | `git ls-files` with a walker fallback |
| D12 | Secrets | Excluded even if tracked, with warnings and an allow-list |
| D13 | Multi-machine proof | Real agent on the 64 GB machine via SSH (sudo allowed) |
| D14 | Identity | Named, hashed, revocable client tokens; jobs record their owner |
| D15 | Tests | Local multi-agent tests + real two-machine validation |

Made during implementation (see ADRs in [ARCHITECTURE.md](ARCHITECTURE.md#11-architectural-decision-records)):

- **Workspace as a Docker volume (ADR-9)** instead of a bind-mounted host directory, which was the
  first-draft default. This avoids root-owned files on the host, host-path differences under
  WSL/Docker Desktop, and host mounts in general.
- **docker CLI rather than the Engine API (ADR-13)**, and Node's built-in `https` rather than undici (ADR-14).
- **TypeScript 6.0** (not 7.0): typescript-eslint does not support TS 7 yet.
- **Agent shutdown reports jobs as `lost`** (not `failed`), so retry policies treat it as an
  infrastructure failure.
- **`controller init --force` moves the old cluster aside** (`<dir>.bak-<timestamp>`) rather than deleting it.
- **act hardening (ADR-15)**, **tar export via `docker save` (ADR-16)**, **extension on the workspace
  host with a message-only webview (ADR-17)**, **IDE feedback from logs and JUnit (ADR-18)**.
- The Dockerfile build-stage field is `stage` (`target` already means node placement).

## Known limitations (current)

- On Windows clients, the executable bit is not captured in snapshots (Linux/macOS are fine).
- If an agent process restarts, its running jobs are lost (and retried if configured); re-attaching to
  surviving containers is a possible later improvement. Network disconnects do *not* lose jobs.
- `secretEnv` values are stored in the controller database (mode 0600) so retries can reproduce the job.
- One controller (no HA); acceptable for a personal LAN.
- Head-of-line: a large high-priority job does not block smaller ones (backfill), but can wait
  indefinitely on a busy cluster.
- Docker builds are not cgroup-limited per job (they run in the worker's Docker daemon); their
  resources are a scheduling reservation only.
- GitHub Actions compatibility is act's: Linux runners only, no OIDC or deployment environments,
  remote reusable workflows and third-party actions are fetched from GitHub, `github.sha` is a
  synthetic commit of the snapshot. Workflows using the `container:` key cannot reach act's
  loopback-bound cache server.
- The extension reads `secretEnv` values from the VS Code server's environment; secrets only set in
  an interactive shell are not visible to it (use the CLI for those).

## Blockers

None.

## Next actions

1. Owner: try the VS Code extension in the Remote SSH window (Run view → ▶ on a task; the dashboard's group view for sharded tasks).
2. Tag `v0.1.0` to publish the first GitHub release (add `NPM_TOKEN` / `VSCE_PAT` secrets first to also publish to npm and the Marketplace).
3. Possible next features: re-attaching to running containers after an agent restart, per-project authorization, a browser dashboard served by the controller, GitHub self-hosted runner integration, Nomad scheduler adapter.
