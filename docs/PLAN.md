# Gridstator: Plan and Progress

Last updated: 2026-11-09. **Current state: Milestones 1 and 2 are implemented and validated, including
across two physical machines on the LAN (Linux laptop controller + WSL2 worker on a Windows PC).
Next: owner review, then Milestone 3.**

## Milestones

| # | Milestone | Status |
|---|---|---|
| 1 | Distributed execution core: controller, agents, CLI, protocol, container executor | ✅ Implemented; validated locally and across two machines |
| 2 | Workspace snapshots: capture, content-addressed transfer, staging, cleanup | ✅ Implemented; validated locally and across two machines |
| 3 | Docker image builds with BuildKit/buildx | ⏳ Next after review |
| 4 | Local GitHub Actions via `act` | Planned |
| 5 | VS Code extension and dashboard | Planned |
| 6 | Advanced scheduling, test sharding, artifacts, retry policies | Partially started (see below) |
| 7 | Open-source release readiness | Partially started (see below) |

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

## Known limitations (current)

- On Windows clients, the executable bit is not captured in snapshots (Linux/macOS are fine).
- If an agent process restarts, its running jobs are lost (and retried if configured); re-attaching to
  surviving containers is a possible later improvement. Network disconnects do *not* lose jobs.
- `secretEnv` values are stored in the controller database (mode 0600) so retries can reproduce the job.
- One controller (no HA); acceptable for a personal LAN.
- Head-of-line: a large high-priority job does not block smaller ones (backfill), but can wait
  indefinitely on a busy cluster.

## Blockers

None.

## Next actions

1. Owner review of M1/M2.
2. Decide the permanent layout (where the controller lives; run controller/agents as services).
3. Milestone 3: `docker-build` executor using `docker buildx build` (context = snapshot volume,
   `--target`, build args, `--cache-from/--cache-to` local cache per worker, image metadata,
   optional `--output` export as artifact, cancellation, labelled cleanup).
4. Milestone 4: `actions` executor via `act` on a materialized snapshot (`--bind` off, explicit
   event payload, inputs/env, unsupported-feature detection, guard against `actions/checkout`
   replacing the snapshot).
5. Milestone 5: VS Code extension (workspace-side extension host, React webview dashboard, Testing
   API and Problems integration).
6. Milestone 6: job groups and Playwright/Vitest sharding, artifacts via `docker cp` into the CAS.
7. Milestone 7: systemd/WSL service helpers, release automation, npm publishing, troubleshooting guide.
