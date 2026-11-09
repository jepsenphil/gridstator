# Gridstator Architecture

> Multiple machines. One workflow.

This document describes how Gridstator is built: its components, how a job flows through the
system, the API and data model, process lifecycles, and the architectural decisions (with the
alternatives considered). It is kept in sync with the implementation. Sections marked
**(planned)** describe designs for later milestones that are not implemented yet; see
[PLAN.md](PLAN.md) for status.

## 1. Goals and non-goals

Gridstator turns a handful of machines on a trusted LAN into a development compute pool. A developer
submits **independent jobs** (a containerized command, a Docker build, a GitHub Actions job via
`act`, a test shard) from VS Code or the CLI; Gridstator snapshots the working tree, ships it to a
suitable worker, runs the job in Docker, and streams results back.

Non-goals: distributing a single process across machines, multi-tenant hosting, replacing GitHub
Actions, Kubernetes/Nomad as a dependency, and treating Docker as a security boundary for untrusted
code.

## 2. Components

```text
 ┌────────────────────────────┐        ┌──────────────────────────────────────────┐
 │ Client (on source host)    │        │ Controller                               │
 │  • gridstator CLI          │ HTTPS  │  • REST API (Fastify)        /v1/...     │
 │  • VS Code extension       ├───────►│  • Client event stream (WS)  /v1/events  │
 │    (workspace host)        │  WSS   │  • Agent channel (WS)        /v1/agent/ws│
 │  • snapshot capture        │◄───────┤  • Scheduler (pure, packages/scheduler)  │
 └────────────────────────────┘        │  • SQLite (node:sqlite): jobs, nodes,    │
                                       │    tokens, snapshots, events             │
                                       │  • CAS blob store (filesystem)           │
                                       │  • Job logs (NDJSON files)               │
                                       └──────────────┬───────────────────────────┘
                                                      │ WSS (agent dials out)
                                  ┌───────────────────┼────────────────────┐
                                  ▼                   ▼                    ▼
                          ┌──────────────┐    ┌──────────────┐     ┌──────────────┐
                          │ Agent        │    │ Agent        │     │ Agent        │
                          │ • metrics    │    │ (64GB box)   │     │ (WSL2 on     │
                          │ • blob cache │    │              │     │  Windows,    │
                          │ • executors  │    │              │     │  validated)  │
                          │ • docker CLI │    │              │     │              │
                          └──────┬───────┘    └──────────────┘     └──────────────┘
                                 ▼
                          Docker Engine / BuildKit / act (local to each worker)
```

There are exactly two long-running process types: **controller** and **agent**. Both ship in the
single `gridstator` npm package (ADR-7), and `gridstator controller start --with-agent` runs both in
one process (ADR-1, "embedded mode").

### 2.1 Repository layout and dependency direction

```text
apps/
  cli/          gridstator binary: client commands + `controller` + `agent` subcommands
  controller/   controller server (library: createController)
  agent/        worker agent (library: createAgent)
packages/
  shared/       errors, ids, logger, paths, durations/sizes, TLS pinning helpers
  protocol/     zod schemas: JobSpec, job states + transitions, REST bodies, WS messages
  scheduler/    pure scheduling function (no I/O)
  snapshot/     working-tree capture, manifests, CAS store, safe materialization
  executors/    Executor interface; docker CLI wrapper; command, docker-build and actions executors
  discovery/    finds workflows (with act caveats), package scripts and Dockerfiles in a project
  client/       typed HTTPS/WSS API client with CA pinning (used by CLI, agent, VS Code)
apps/vscode/    VS Code extension: extension host (src/) and React dashboard webview (webview/)
examples/       sample projects used for manual and automated validation
```

Dependencies only point downward: `apps/* → client, executors, snapshot, scheduler → protocol →
shared`. `scheduler` and `protocol` perform no I/O and are unit-tested in isolation.

### 2.2 Controller

Responsibilities: authentication, token and node registry, job queue and state machine, scheduling,
dispatch to agents, reconciliation, log persistence and fan-out, snapshot/blob storage, retention.

The controller is the single source of truth. All job state transitions happen in the controller
inside SQLite transactions; agents *report* facts (started, exited with code N) and the controller
decides the resulting state.

### 2.3 Agent

Responsibilities: report node info and live metrics, maintain the outbound WebSocket, fetch
snapshots into a local blob cache, run jobs through executors, stream logs, enforce timeouts and
cancellation, clean up Docker resources it owns, and buffer results while disconnected.

The agent never accepts inbound connections. It dials the controller (ADR-4), which works from
WSL2, behind host firewalls, and on laptops whose address changes.

### 2.4 Clients

The CLI and the VS Code extension share `packages/client`, `packages/snapshot` and
`packages/discovery`. Snapshot
capture runs **on the machine that hosts the source** — with VS Code Remote SSH that is the
workspace extension host on the Linux laptop, never the Windows UI machine.

## 3. Execution flow

```text
client                      controller                         agent                 docker
  │ capture snapshot           │                                  │                     │
  │ (git ls-files, hash)       │                                  │                     │
  │── POST /v1/blobs/missing ─►│                                  │                     │
  │◄── missing digests ────────│                                  │                     │
  │── POST /v1/blobs/upload ──►│ verify sha256 of each entry      │                     │
  │   (tar of missing blobs)   │                                  │                     │
  │── POST /v1/snapshots ─────►│ verify manifest + all blobs      │                     │
  │── POST /v1/jobs ──────────►│ CREATED → QUEUED                 │                     │
  │                            │ scheduler picks node             │                     │
  │                            │ QUEUED → ASSIGNED ── job.assign ►│                     │
  │                            │                                  │ fetch missing blobs │
  │                            │◄────────── job.state starting ───│ (local CAS cache)   │
  │                            │ → STARTING                       │── volume create ───►│
  │                            │                                  │── create container ►│
  │                            │                                  │── docker cp (tar) ─►│
  │                            │◄────────── job.state running ────│── start -a ────────►│
  │◄═ WS log stream ═══════════│◄══════════ job.log (batched) ════│◄═ stdout/stderr ════│
  │                            │◄── job.state exited(code) ───────│── inspect, rm ─────►│
  │                            │ → SUCCEEDED / FAILED             │                     │
  │                            │── job.ack ──────────────────────►│ forget result       │
```

### 3.1 Workspace snapshots (M2)

1. **Root and workdir.** The project root is the nearest ancestor containing `gridstator.yml`,
   otherwise the git top-level, otherwise the current directory. The job's working directory is the
   caller's path relative to that root.
2. **File set.** `git ls-files -z --cached --others --exclude-standard` (ADR-11), recursing into
   submodules; tracked files deleted in the working tree are skipped. Outside git, a walker applying
   `.gitignore` files is used. Then built-in excludes (`.git/`, `node_modules/`), secret patterns
   (ADR-12), and `gridstator.yml` `snapshot.exclude` are removed; `snapshot.include` may add
   otherwise-ignored paths, and `snapshot.allowSecrets` re-admits specific secret-pattern files.
3. **Entries.** Regular files are hashed (SHA-256) using a local stat cache
   (size + mtime + ctime + inode) so unchanged files are not re-read. Symlinks are recorded with
   their target; symlinks that are absolute or escape the root are excluded with a warning. Only the
   executable bit of the mode is kept.
4. **Manifest and ID.** Entries sorted by byte-wise path form a canonical JSON manifest; the
   **snapshot ID is the SHA-256 of that manifest** (`sha256:<hex>`). Identical trees always produce
   the same ID; any byte change produces a new ID. Jobs reference an immutable snapshot ID.
5. **Consistency.** After hashing, every file is re-stat'ed; if anything changed during capture,
   capture is retried. On upload the controller re-hashes each blob and rejects mismatches, so a
   stored snapshot always matches its ID exactly.
6. **Transfer.** The client asks which digests the controller lacks and uploads only those as a
   single tar stream. Agents keep their own blob cache and fetch only what they lack the same way.
7. **Staging.** The agent streams a tar built from the manifest and its cache directly into the job's
   Docker volume (`docker cp -`), so no per-job host directory is needed (ADR-9). For executors that
   need a host directory (`docker-build`, `actions`), the hardened extractor writes into a fresh per-job directory
   (no `..`, no absolute paths, no symlink traversal, no overwrite of existing entries).
8. **Unsaved buffers.** The VS Code extension (M5) will block or prompt when dirty editors exist,
   because snapshots capture what is on disk.

### 3.2 Container command executor (M1)

For each job the agent:

1. creates volume `gridstator-ws-<jobId>`;
2. `docker create --pull missing --init --name gridstator-<jobId>` with labels
   `io.gridstator.managed=true`, `io.gridstator.job=<jobId>`, `io.gridstator.node=<nodeId>`,
   `--cpus`, `--memory` (swap disabled), `--pids-limit`, `--security-opt no-new-privileges`, the
   workspace volume at `/workspace`, the shared cache volume at `/gridstator/cache`, and an
   `--env-file` written with mode 0600 and deleted immediately after create (secrets never appear in
   argv; multi-line values are passed by name through the docker CLI's own environment). Jobs see
   `CI=true`, `GRIDSTATOR_JOB_ID`, `GRIDSTATOR_ATTEMPT`, `GRIDSTATOR_NODE_ID`, `GRIDSTATOR_NODE_NAME`;
3. streams the snapshot tar into `/workspace`;
4. `docker start --attach`, streaming stdout/stderr (secret values masked);
5. `docker inspect` for the exact exit code and OOM flag;
6. removes the container and volume (unless the retention policy keeps failed workspaces).

Cancellation and timeouts use `docker stop --time <grace>` (SIGTERM to the init process, which
forwards it to the process tree, then SIGKILL). The agent only ever touches containers and volumes
carrying `io.gridstator.managed=true` **and** its own node ID.

**Dependency caches (ADR-10).** The volume `gridstator-cache` is mounted at `/gridstator/cache` and
environment variables point common package managers at it (`npm_config_cache`,
`npm_config_store_dir` for pnpm, `YARN_CACHE_FOLDER`, `COREPACK_HOME`, `PIP_CACHE_DIR`,
`GOMODCACHE`, `GOCACHE`). The job's command performs the install; with a warm cache this is fast.

### 3.3 Docker build executor (M3)

`docker-build` jobs stage the snapshot into the job's private work directory (hardened
materializer) and run `docker buildx build --progress=plain` with:

- the Dockerfile and context resolved **inside** the staged snapshot (paths that resolve outside it,
  including through symlinks, are rejected);
- `--target`, `--build-arg`, `--tag`, `--platform`, `--no-cache`, `--pull`, `--cache-from/--cache-to`
  passed through; **build secrets** as `--secret id=NAME,env=NAME` with the value only in the buildx
  process environment;
- image labels `io.gridstator.managed=true`, `io.gridstator.job`, `io.gridstator.node`;
- `--metadata-file`, from which the image ID and digest are reported as job `outputs.image`.

Outputs: `load` (into the worker's image store, the default), `push` (to the registry named by the
tags), `tar` (the image is loaded, `docker save`d and uploaded as the `image.tar` artifact — ADR-16),
or `none` (build only: validates the Dockerfile and warms the cache). The BuildKit cache stays in
the worker's daemon; the scheduler's project affinity sends repeat builds to the same worker.
Cancellation sends SIGINT to buildx (which cancels the BuildKit session), then SIGKILL after a grace
period.

### 3.4 GitHub Actions executor (M4)

`actions` jobs run [nektos/act](https://github.com/nektos/act) against the snapshot:

1. The snapshot is staged into the job work directory and turned into a git repository with one
   commit, on the developer's branch name and with their `origin` URL (captured by the client), so
   `github.ref`, `github.ref_name` and `github.repository` match their checkout. `github.sha` is the
   synthetic commit's.
2. act runs `act <event> -C <src> -W <workflow> [-j <job>] --json --rm --pull=false` with a private
   `HOME` (action clones and caches persist per worker) and these defaults (ADR-15):
   - `--container-daemon-socket -`: the worker's Docker socket is **not** mounted into job
     containers unless the job opts in (`dockerSocket`), and workflow-supplied container options and
     host-path volumes are restricted;
   - `--secret-file/--var-file/--env-file` point at private 0600 files written from the job spec, so
     act never reads `.secrets`, `.vars` or `.env` from the project;
   - `--cache-server-addr 127.0.0.1` (act binds it to the LAN address by default) and a per-job
     `--artifact-server-port`, so concurrent workflow jobs do not collide;
   - `--container-options "--label io.gridstator.…"`, so act's containers carry the job labels and
     can be cleaned up after a crash;
   - runner images per `runs-on` label (`-P`), defaulting to `catthehacker/ubuntu:act-*`.
3. `actions/checkout` of the current repository is skipped by act, which copies the staged files
   instead — the workflow sees exactly the submitted snapshot. Checkout of *other* repositories
   clones from GitHub.
4. act's JSON log stream is rendered in act's familiar console format and parsed into structured
   **steps** (`outputs.steps`), published live with `job.outputs` messages.
5. Files uploaded with `actions/upload-artifact` (act's artifact server) become job artifacts.

The whole workflow runs in one act invocation on one worker, so `needs:` ordering follows act's
behaviour; `--job` runs a single job without its dependencies. Client-side discovery
(`packages/discovery`) lists workflows, jobs, events and `workflow_dispatch` inputs and flags
features act cannot reproduce (Windows/macOS runners, OIDC, deployment environments, remote
reusable workflows, Docker usage without the socket, `GITHUB_TOKEN`).

### 3.5 Artifacts and structured outputs

Executors attach **artifacts** by uploading files to the controller's content-addressed store (agents
may upload and query blobs) and listing `{name, digest, size}` in the job result; the controller
records them in the `artifacts` table and serves them at `/v1/jobs/:id/artifacts/<name>`.
Command jobs collect `artifacts` paths from the workspace volume (`docker cp container:path -`, one
tar per path); build jobs export `image.tar`; workflow jobs return act's uploads. Artifact blobs are
kept by the retention mark-and-sweep for as long as their job exists.

**Outputs** (`jobs.outputs`) hold structured results beyond the exit code: `image` (ID, digest,
tags, size, output mode) and `steps` (name, group, status, start, duration). Agents publish them
while the job runs (`job.outputs`) and with the final result.

### 3.6 VS Code extension (M5)

The extension declares `extensionKind: workspace`, so under Remote SSH it runs on the machine that
hosts the source. It reuses the CLI's client configuration (and `gridstator login`), and talks to
the controller with the same pinned-TLS client.

| Part | Responsibility |
|---|---|
| `ClusterState` | connection, nodes and recent jobs kept live from `/v1/events`; reconnects with backoff |
| `Launcher` | discovers tasks, builds, workflows, package scripts and Dockerfiles; checks unsaved editors; captures, uploads and submits |
| Tree views | Activity Bar: Cluster (nodes and node actions), Jobs (running, queued, recent; cancel/retry), Run (everything runnable) |
| `Dashboard` | one React webview: overview, jobs, job details with live logs, run launcher, node management |
| `ResultsIntegration` | Problems panel from job logs; Testing view from JUnit artifacts; a *Run on Gridstator* test profile |
| Status bar | nodes online, running and queued jobs; sign-in/offline states |

The webview has a strict Content Security Policy (nonce-locked script, local resources only) and
**never** contacts the controller: it sends typed messages (`src/messages.ts`) to the extension,
which owns the client, the user's files and dialogs. Remote paths in logs and JUnit reports
(`/workspace/…` in containers, act's staging directory) are mapped back to workspace files for
Problems, Testing locations and clickable log links.

## 4. Job model and state machine

```text
CREATED ─► QUEUED ─► ASSIGNED ─► STARTING ─► RUNNING ─┬─► SUCCEEDED
              ▲          │           │          │      ├─► FAILED
              └──────────┘           │          │      ├─► CANCELLED
          (agent rejected / lost     │          │      └─► LOST
           before starting)          └──────────┴─► FAILED / CANCELLED / LOST
 (any non-terminal state) ─► CANCELLED
```

API values are lowercase (`queued`, `running`, …). The allowed-transition table lives in
`packages/protocol/src/states.ts` and is the only place transitions are defined. Rules:

- **Idempotent reports.** Agent messages carry `jobId` + `assignmentId`. Messages for a stale
  assignment, a different node, or a transition not in the table are ignored and logged. Repeating a
  state report is a no-op. Log entries carry a per-job sequence number; entries at or below the
  persisted high-water mark are dropped, so agents can safely replay after reconnecting.
- **Terminal states are final.** A retry creates a *new* job (`retryOf`, `attempt + 1`) with the same
  spec and snapshot, so history is never rewritten.
- **Cancellation.** Before dispatch: immediate `cancelled`. After dispatch: `cancelRequestedAt` is set
  and `job.cancel` sent; the agent stops the container and reports. If the agent does not confirm
  within the cancel timeout, or is offline, the controller finalizes `cancelled` and the agent is told
  to clean up when it reconnects. Reservations are released on any terminal state.
- **Lost work.** If a node's channel drops, its jobs get a grace period (`agentLostTimeout`, default
  60 s) to reconnect. Afterwards `assigned` jobs return to `queued`; `starting`/`running` jobs become
  `lost`. Automatic retry (`retry.max`, `retry.on: [lost|failed]`) then creates a new attempt.

### 4.1 Job record

| Field | Meaning |
|---|---|
| `id` | ULID-style, time-sortable (`01J…`) |
| `name`, `kind` | display name; `command`, `docker-build` or `actions` |
| `spec` | validated `JobSpec` (image, command, env, resources, target, timeout, retry, snapshot, workdir) |
| `state`, `stateReason` | current state and human-readable reason (e.g. "waiting for capacity") |
| `priority` | higher runs first; FIFO within a priority |
| `owner` | name of the client token that submitted the job (audit) |
| `nodeId`, `assignmentId` | current/last assignment |
| `snapshotId` | immutable source snapshot, or null |
| `attempt`, `retryOf` | retry lineage |
| `exitCode`, `error` | result details |
| `createdAt`, `queuedAt`, `assignedAt`, `startedAt`, `finishedAt`, `cancelRequestedAt` | timestamps |
| `logSeq` | highest persisted log sequence number |
| `outputs` | structured results: `image`, `steps` (§3.5) |
| `artifacts` | `{name, digest, size}` files produced by the job (§3.5) |

Every transition is also appended to `job_events` (who/what caused it, when), which forms the audit
trail.

## 5. Scheduling

`packages/scheduler` exports a pure function:
`schedule(queue, nodes, context) → { assignments, pending: Map<jobId, reason> }`.

1. Jobs are ordered by priority (desc), then queue time (asc).
2. A node is **eligible** for a job when it is online, its scheduling mode is `enabled` (not
   `disabled` or `draining`), it has the required capabilities (`docker`, …), it carries all required
   labels, it matches an explicit `target.node` if given, it has a free concurrency slot, and the job's
   resource request fits in `allocatable = total − reserved − Σ(requests of active jobs)`. Live
   metrics are also checked: a node whose currently available memory is below the request is skipped.
   This is what keeps the development laptop usable (ADR-6).
3. Among eligible nodes, the highest score wins: free CPU and memory fraction, minus a penalty for
   current CPU utilization, plus a bonus if the node recently ran a job from the same project (warm
   blob and dependency caches).
4. Lower-priority jobs may backfill when a higher-priority job cannot currently fit. Each pending job
   gets a reason (`no node has label "gpu"`, `waiting for capacity`) shown to the user.

The scheduler runs on job submission, job completion, node changes, and a periodic tick. Placing the
algorithm behind a pure function is also the seam for an optional future Nomad adapter.

## 6. API

All endpoints are under `/v1`, served over TLS, JSON unless noted, and require
`Authorization: Bearer <token>` except `GET /v1/health` and `GET /v1/pki/ca.pem`. See
[API.md](API.md) for request and response details.

| Area | Endpoints |
|---|---|
| Health/PKI | `GET /health`, `GET /pki/ca.pem` |
| Cluster | `GET /cluster` |
| Nodes | `GET /nodes`, `GET /nodes/:id`, `PATCH /nodes/:id`, `DELETE /nodes/:id` |
| Agents | `POST /agents/register` (join token), `GET /agent/ws` (agent credential) |
| Jobs | `POST /jobs`, `GET /jobs`, `GET /jobs/:id`, `GET /jobs/:id/events`, `GET /jobs/:id/logs`, `POST /jobs/:id/cancel`, `POST /jobs/:id/retry` |
| Blobs | `POST /blobs/missing`, `POST /blobs/upload` (tar), `POST /blobs/download` (tar), `GET /blobs/:digest` |
| Snapshots | `POST /snapshots`, `GET /snapshots/:id` |
| Tokens | `POST /tokens`, `GET /tokens`, `DELETE /tokens/:id` (admin) |
| Events | `GET /events` (WebSocket: job/node updates, log subscriptions) |
| Artifacts | `GET /jobs/:id/artifacts`, `GET /jobs/:id/artifacts/<name>` |

### 6.1 Agent channel protocol

One WebSocket per agent, JSON messages with a `type` discriminator, validated with zod on receipt.

| Direction | Message | Purpose |
|---|---|---|
| A→C | `hello` | node info, agent/protocol version, explicitly-set settings, active jobs, unacknowledged results |
| C→A | `welcome` | effective node settings, per-job resume points (`logSeq`), jobs to cancel |
| A→C | `heartbeat` | live metrics, active job IDs (every 5 s) |
| C→A | `job.assign` | job ID, assignment ID, spec, snapshot ID |
| A→C | `job.state` | `starting` / `running` |
| A→C | `job.result` | terminal outcome `succeeded` / `failed` / `cancelled` / `lost` (agent shut down), exit code, OOM flag, error, final log sequence |
| A→C | `job.log` | batched log entries with per-job sequence numbers |
| A→C | `job.outputs` | structured progress (e.g. workflow steps) while the job runs |
| A→C | `job.rejected` | the agent cannot run the job (e.g. Docker unavailable) |
| C→A | `job.cancel` | stop the job |
| C→A | `job.ack` | the controller persisted a terminal result; the agent may forget it |
| C→A | `node.settings` | settings changed through the API |

Protocol version mismatches are rejected in `welcome` with an actionable error.

## 7. Persistence and storage

Controller data directory (default `$XDG_DATA_HOME/gridstator/controller`, mode 0700):

```text
controller.json        listen host and port
controller.db          SQLite (WAL, schema v2): meta, tokens, nodes, jobs (+outputs), job_events,
                       snapshots, artifacts
pki/ca.pem, ca.key     cluster CA (key mode 0600)
pki/server.pem, .key   controller TLS certificate, signed by the CA
cas/sha256/ab/cdef…    content-addressed blobs (file contents and manifests)
logs/<jobId>.ndjson    job logs: {"s":seq,"t":ms,"o":"stdout|stderr|system","d":"…"}
```

Agent data directory (default `$XDG_DATA_HOME/gridstator/agent`):

```text
agent.json             controller URL, CA, node ID, agent credential (mode 0600)
cas/…                  blob cache (size-capped, least-recently-used eviction)
results/<jobId>.json   terminal results not yet acknowledged by the controller
logs/<jobId>.ndjson    local log spool used for replay after reconnect
tmp/                   short-lived 0600 env/secret files (deleted right after use)
work/<jobId>/          per-job staging for builds and workflows (removed when the job ends)
act-home/              private HOME for act: action clones, tool and cache-server data
```

**Retention.** Hourly sweeps delete logs of terminal jobs older than `logRetentionDays`, job rows
older than `jobRetentionDays`, snapshots unused for `snapshotRetentionDays`, and unreferenced blobs.
Agents remove job containers and volumes when the job finishes and garbage-collect stray managed
resources on start-up.

## 8. Process lifecycle

**Controller start.** Open the DB and migrate the schema; load the PKI; mark every node offline;
start the HTTPS server; jobs in `assigned`/`starting`/`running` wait for their agents to reconnect
(the lost-grace timer starts now); start scheduler and retention timers. Shutdown on SIGINT/SIGTERM
stops accepting connections, closes agent channels (agents keep running their jobs), flushes logs,
and closes the DB.

**Controller restart reconciliation.** When an agent says `hello`, it lists active jobs and
unacknowledged results. For each job the controller thinks is on that node:

| Controller state | Agent reports | Action |
|---|---|---|
| active | running | keep; resume log streaming from the persisted `logSeq` |
| active | finished result | apply result, `job.ack` |
| `assigned` | unknown | requeue |
| `starting`/`running` | unknown | `lost` (retry policy may apply) |
| terminal | running | `job.cancel` (e.g. cancelled while disconnected) |

**Agent start.** Load `agent.json`; probe Docker/buildx/act; remove managed containers and volumes
left by a previous agent process (the controller marks those jobs `lost` when the agent reconnects
without them, and retry policies apply); connect with exponential backoff (1 s → 30 s with jitter).
Network disconnects do **not** stop running jobs: the agent keeps spooling logs and results and
replays them on reconnect. Shutdown (SIGINT/SIGTERM) stops accepting work and stops running jobs,
reporting them as `lost` so retry policies apply; `agent start --drain` waits for them instead.

## 9. Security and registration (summary)

See [SECURITY.md](SECURITY.md) for the threat model.

- `controller init` creates a cluster CA (ECDSA P-256) and a server certificate. Connection strings
  carry the **SHA-256 fingerprint of the CA**. On first contact a client or agent downloads the CA
  certificate, verifies its fingerprint, stores it, and from then on uses it as the *only* trusted
  root (normal chain verification; the hostname check is skipped because the pinned private CA is
  the trust anchor and LAN addresses change).
- Tokens are 256-bit random values stored as SHA-256 hashes: **client tokens** (named, role
  `admin` or `operator`, revocable), **join tokens** (expiring, limited uses), and **agent
  credentials** (one per node, issued at registration, revoked when a node is removed).
- Pairing: `gridstator token create --join` → on the worker
  `gridstator agent join <url> <join-token>` → the agent registers, receives its credential, and
  saves it.

## 10. Deployment model

- **Install:** `npm install -g gridstator` (Node ≥ 22.19) on each machine. Workers also need Docker
  (BuildKit/buildx for builds, `act` for workflows) accessible to the agent's user.
- **Controller:** `gridstator controller init` then `gridstator controller start [--with-agent]`.
  Service installation helpers (systemd units, Windows/WSL guidance) are planned for M7.
- **Workers:** `gridstator agent join …` then `gridstator agent start` (or as a systemd unit).
- **Clients:** `gridstator login <url> --fingerprint … --token …`, or the VS Code extension's
  sign-in command (M5).
- Ports: controller listens on `0.0.0.0:7300` by default (configurable). Agents and clients only
  make outbound connections.

## 11. Architectural decision records

| ID | Decision | Alternatives considered | Rationale |
|---|---|---|---|
| ADR-1 | One binary; controller and agent can run in one process | dedicated controller host | lowest install friction; any node can host the controller |
| ADR-2 | Snapshots via the controller's content-addressed store | direct client→agent transfer; rsync over SSH | one authenticated endpoint, dedupe across jobs, cache-aware scheduling; no pairwise SSH trust |
| ADR-3 | Self-generated CA with fingerprint pinning; bearer tokens | plain HTTP; WireGuard/Tailscale; mTLS | encryption and server authentication with zero manual certificate work; tokens are simpler than client certificates and easy to revoke |
| ADR-4 | Agents dial the controller over one WebSocket | controller dials agents; gRPC | works through NAT/WSL/firewalls; one multiplexed channel; no inbound ports on workers |
| ADR-5 | Containers only for command execution | host processes | reproducible, resource-limited, clean process-tree termination; host execution may come later as an explicit per-worker opt-in |
| ADR-6 | Every node (including the dev laptop) is a normal worker with reserved CPU/RAM and live-memory checks | manual-only node | uses idle capacity without starving interactive work |
| ADR-7 | Single npm package `gridstator`, bundled with esbuild | separate packages; Docker images | one install everywhere; agent needs direct host Docker access |
| ADR-8 | `node:sqlite` for persistence | better-sqlite3 | no native compilation, which matters for a globally installed CLI on mixed machines |
| ADR-9 | Per-job **Docker volume** workspace populated via `docker cp` | bind-mounted host directory | no root-owned files on the host, no host-path differences (WSL/Docker Desktop), cleanup is one `volume rm`, and there are no host mounts at all |
| ADR-10 | Shared cache volume + package-manager env vars; job runs install | auto-install step; dependency image layers | language-agnostic, explicit, fast when warm |
| ADR-11 | `git ls-files` decides the file set | own ignore walker | exactly matches git semantics, including nested and global ignores |
| ADR-12 | Secret-pattern files excluded even when tracked, with warnings; explicit allow-list | trust git; block the job | prevents leaking committed secrets without blocking work |
| ADR-13 | Drive Docker through the `docker` CLI | dockerode / raw Engine API | respects Docker contexts and `DOCKER_HOST`, identical behaviour to what users debug by hand, buildx is CLI-only anyway, no native dependencies |
| ADR-14 | Node's built-in `https`, `ws` for WebSockets | undici, axios | fewer dependencies; works unchanged inside the VS Code extension host |
| ADR-15 | act runs with the Docker socket off, private secret/var/env files, loopback cache server, labelled containers | act defaults | act's defaults mount the host Docker socket, read secrets from the project and expose its cache server on the LAN |
| ADR-16 | `tar` image output via `--load` + `docker save` | buildx `--output type=docker,dest=…` | the default `docker` build driver cannot export to a file |
| ADR-17 | Extension runs on the workspace host; the webview only messages the extension | webview calling the API directly | snapshots must be captured where the files are; keeps credentials and pinned TLS out of the webview |
| ADR-18 | IDE feedback from logs (problem matchers) and JUnit artifacts | custom test protocol | works with any language or test runner that can print errors or write JUnit |

## 12. Extension points

- **Executors** implement `Executor { kind; run(ctx) }` with a context that provides snapshot
  staging, artifact upload and progress reporting; new job kinds plug in beside `command`,
  `docker-build` and `actions`.
- **Job groups** (planned, M6) add `groupId` so shards and matrix jobs are tracked together.
- **Scheduler adapters**: the pure `schedule()` boundary allows an optional external scheduler later.
