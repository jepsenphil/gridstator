# Controller API (v1)

The controller exposes a versioned HTTPS API under `/v1` and two WebSocket endpoints. The CLI, the
agents and the (planned) VS Code extension all use it through `packages/client`. The zod schemas in
`packages/protocol` are the source of truth for every request and response shape; this document
summarizes them.

## Conventions

- **Transport:** HTTPS only. Clients trust only the cluster CA, pinned by its SHA-256 fingerprint
  (see [SECURITY.md](SECURITY.md)). Hostname verification is not performed; chain verification is.
- **Authentication:** `Authorization: Bearer <token>` on every endpoint except `GET /v1/health` and
  `GET /v1/pki/ca.pem`. Token kinds: `gsc_` client (role `operator` or `admin`), `gsj_` join,
  `gsa_` agent.
- **Bodies:** JSON (`application/json`) unless noted. Timestamps are epoch milliseconds.
- **Errors:** non-2xx responses carry

  ```json
  { "error": { "code": "NOT_FOUND", "message": "Job 01M6… not found", "hint": "…optional…" } }
  ```

  | Code | HTTP | Meaning |
  |---|---|---|
  | `BAD_REQUEST` | 400 | invalid input; the message lists the failing fields |
  | `UNAUTHORIZED` | 401 | missing, invalid, expired, revoked or used-up token |
  | `FORBIDDEN` | 403 | valid token of the wrong kind or role |
  | `NOT_FOUND` | 404 | unknown job, node, snapshot, blob, token or route |
  | `CONFLICT` | 409 | illegal in the current state (e.g. retrying a running job) |
  | `PAYLOAD_TOO_LARGE` | 413 | upload over limits |
  | `INTERNAL` | 500 | controller bug; see controller logs |

## Endpoints

### Health and PKI (no authentication)

| Method & path | Response |
|---|---|
| `GET /v1/health` | `{ ok: true, service: "gridstator-controller", version, apiVersion: "v1" }` |
| `GET /v1/pki/ca.pem` | the cluster CA certificate (PEM). Clients must verify its fingerprint. |

### Cluster and settings (client)

| Method & path | Notes |
|---|---|
| `GET /v1/cluster` | `{ clusterId, version, build: ControllerBuild, caFingerprint, startedAt, nodes: {total, online}, jobs: {<state>: count}, whoami: {name, role} }` |
| `GET /v1/settings` | `{ jobRetentionDays, logRetentionDays, snapshotRetentionDays, autoUpdate: "off"\|"follow"\|"window", updateWindow: {start, end} \| null }` |
| `PATCH /v1/settings` | admin; partial update of the above (`window` mode needs `updateWindow`, times are `HH:MM` controller local time) |

`ControllerBuild`: `{ id, version, sha256, updatable, reason }`. `id` is `<version>-<first 12 hex of
sha256>` (`<version>-unbundled` for a workspace build, which workers cannot update to).

### Worker updates

| Method & path | Auth | Notes |
|---|---|---|
| `GET /v1/updates` | client | `{ controller: ControllerBuild, autoUpdate, updateWindow, rollout: Rollout \| null }` |
| `POST /v1/updates` | admin | body: `{ nodes?: string[], now?: boolean, drainTimeoutSeconds?: number }`; starts a rollout (or adds to the running one) → same as `GET` |
| `POST /v1/updates/cancel` | admin | halts the running rollout |
| `POST /v1/controller/build?sha256=<hex>` | admin | body: a gridstator `.tgz` (`application/gzip`); the controller installs it and restarts on it → `{ action: "none"\|"restarting", from, to }` |

`GET /v1/updates` also returns `toolchain: [{ name: "act"|"node", version, source:
"pinned"|"override"|"off", platforms: [{ platform, state: "ready"|"preparing"|"failed", reason }]
}]`; `PATCH /v1/settings` takes `toolchain: { act, node }` (each `null` for the build's pin, `"off"`,
or a version).

`Rollout`: `{ id, target, state: "running"|"halted"|"finished", haltReason, startedBy, startedAt,
finishedAt, now, drainTimeoutSeconds, nodes: [{ nodeId, name, state:
"pending"|"draining"|"updating"|"done"|"failed"|"skipped", detail, auto, startedAt, finishedAt }] }`.

Agents use three routes whose shape never changes, so agents of any version can update:

| Method & path | Auth | Notes |
|---|---|---|
| `GET /v1/agent/update?current=<build id>&mode=normal\|update-only` | agent | `{ action: "none", reason }` or `{ action: "update", build, package: {sha256, size}, confirmWithinSeconds }` |
| `GET /v1/agent/update/package/:buildId` | agent | the build as an npm-style `.tgz` |
| `POST /v1/agent/update/report` | agent | body: `{ target, error }`: the update could not be installed |

Toolchain archives (not part of the frozen routes): `GET /v1/agent/toolchain?platform=&arch=` →
`{ tools: [{ name, version, platform, state, sha256, size, bin, reason }] }`, and
`GET /v1/agent/toolchain/:name/:version/:platform` → the `.tar.gz`.

### Nodes

| Method & path | Auth | Notes |
|---|---|---|
| `GET /v1/nodes` | client | `{ nodes: Node[] }` |
| `GET /v1/nodes/:idOrName` | client | `Node` |
| `PATCH /v1/nodes/:idOrName` | client | body: `{ maxConcurrency?, reserved?: {cpus?, memoryMb?}, labels?, schedulingMode?: "enabled"\|"disabled"\|"draining", autoUpdate?: "inherit"\|"off"\|"follow"\|"window" }` |
| `DELETE /v1/nodes/:idOrName[?force=true]` | admin | revokes the agent credential; 409 if jobs are active unless `force` |
| `POST /v1/agents/register` | join | body: `{ name, info: NodeInfo, settings? }` → `{ nodeId, name, credential }`; consumes one use of the join token |

`Node` fields: `id`, `name`, `status` (`online`/`offline`), `health` (`ok`/`degraded`/`unknown`),
`healthIssues[]`, `info` (hostname, platform, arch, cpus, memoryTotalMb, docker/buildx/act status,
executors), `metrics` (cpuPercent, memoryAvailableMb, diskFreeMb, diskTotalMb, load1),
`settings`, `effectiveLabels` (automatic `os=…`, `arch=…`, `docker`, `buildx`, `act` plus user
labels), `activeJobs[]`, `allocated`, `allocatable`, `lastSeenAt`, `connectedAt`, `createdAt`,
`update` (`{ state: "current"|"behind"|"updating"|"failed"|"unknown", build, detail, autoUpdate,
canUpdate }`, compared against the controller's build). `info.build` is the agent's
`{ id, version, sha256, managed, node, lastRollback }`.

### Jobs (client)

| Method & path | Notes |
|---|---|
| `POST /v1/jobs` | body `{ spec: JobSpec }` → 201 `Job` |
| `GET /v1/jobs?state=a,b&node=&limit=&before=` | newest first; `before` is a job id cursor → `{ jobs: Job[] }` |
| `GET /v1/jobs/:id` | `Job` |
| `GET /v1/jobs/:id/events` | `{ events: JobEvent[] }`, the audit trail of state changes |
| `GET /v1/jobs/:id/logs?after=&limit=` | `{ entries: LogEntry[], complete }` |
| `POST /v1/jobs/:id/cancel` | idempotent; returns `Job` (terminal, or with `cancelRequestedAt` set) |
| `POST /v1/jobs/:id/retry` | terminal jobs only; 201 new `Job` with `attempt + 1`, `retryOf` |
| `GET /v1/jobs/:id/artifacts` | `{ artifacts: [{ name, digest, size }] }` |
| `GET /v1/jobs/:id/artifacts/<name>` | the artifact's bytes (`application/octet-stream`, `content-disposition: attachment`); `name` may contain `/` |

`JobSpec` is one of three kinds. All share `name`, `env`, `secretEnv`, `snapshot`, `workdir`,
`resources`, `target`, `timeoutSeconds`, `retry`, `priority` and `project`.

Kind `command`:

```jsonc
{
  "kind": "command",
  "name": "unit tests",                  // optional display name
  "image": "node:24-bookworm",            // default
  "command": ["sh", "-c", "npm test"],    // argv
  "env": { "NODE_ENV": "test" },
  "secretEnv": { "NPM_TOKEN": "…" },      // masked in logs, redacted in responses
  "snapshot": "sha256:…",                 // optional source snapshot
  "workdir": "packages/web",              // relative to the snapshot root, default "."
  "resources": { "cpus": 2, "memoryMb": 4096 },
  "target": { "node": "big-box", "labels": ["fast"], "prefer": [] },  // prefer: soft node preference
  "timeoutSeconds": 1800,
  "retry": { "max": 1, "on": ["lost"] },  // "lost" and/or "failed"
  "priority": 0,                           // -100..100, higher first
  "project": "web-app",                    // cache-affinity key
  "artifacts": ["coverage", "reports/junit.xml"]  // collected as <path>.tar artifacts
}
```

Kind `docker-build` (BuildKit on the worker; `secretEnv` entries become `--secret id=NAME,env=NAME`):

```jsonc
{
  "kind": "docker-build",
  "snapshot": "sha256:…",
  "context": ".",                          // relative to the snapshot root
  "dockerfile": "apps/web/Dockerfile",     // default <context>/Dockerfile
  "stage": "runner",                       // --target
  "buildArgs": { "APP_VERSION": "1.2.3" },
  "tags": ["web:dev"],
  "platforms": [],
  "noCache": false,
  "pull": false,
  "output": "load",                        // load | push | tar (image.tar artifact) | none
  "cacheFrom": [], "cacheTo": []
}
```

Kind `actions` (nektos/act on the worker):

```jsonc
{
  "kind": "actions",
  "snapshot": "sha256:…",
  "workflow": ".github/workflows/ci.yml",
  "job": "test",                           // omit for the whole workflow
  "event": "workflow_dispatch",
  "inputs": { "greeting": "hi" },
  "vars": {}, "env": {},
  "platforms": { "ubuntu-latest": "node:24-bookworm-slim" },  // runs-on label → image
  "matrix": ["node:22"],
  "dockerSocket": false,                   // mount the worker's Docker socket into job containers
  "eventPayload": {},                      // optional github.event
  "git": { "branch": "main", "remoteUrl": "https://github.com/acme/web.git" }
}
```

`Job` adds: `id`, `name`, `kind`, `state`, `stateReason`, `priority`, `owner`, `spec` (redacted),
`snapshotId`, `nodeId`, `nodeName`, `assignmentId`, `attempt`, `retryOf`, `exitCode`, `error`,
`queuePosition` (queued jobs only), `logSeq`, `outputs`, `artifacts`, `groupId`, `shardIndex`,
`shardTotal`, `createdAt`, `queuedAt`, `assignedAt`, `startedAt`,
`finishedAt`, `cancelRequestedAt`.

`outputs` is `null` or `{ image?: { id, digest, tags, sizeBytes, output }, steps?: [{ id, group,
name, status, startedAt, durationMs }] }`; step `status` is `running`, `success`, `failure`,
`skipped` or `cancelled`. `artifacts` is a list of `{ name, digest, size }`.

States: `created`, `queued`, `assigned`, `starting`, `running`, `succeeded`, `failed`, `cancelled`,
`lost` (see [ARCHITECTURE.md §4](ARCHITECTURE.md#4-job-model-and-state-machine)).

`LogEntry`: `{ s: seq, t: epochMs, o: "stdout"|"stderr"|"system", d: text }`.

### Job groups (client)

| Method & path | Notes |
|---|---|
| `POST /v1/groups` | `{ spec: JobSpec (kind command), shards: 1–64, name? }` → 201 `JobGroup`. Shard *i* gets `{shard}`/`{total}` replaced in its command and `GRIDSTATOR_SHARD_INDEX`/`GRIDSTATOR_SHARD_TOTAL` in its environment. |
| `GET /v1/groups?limit=&before=` | `{ groups: JobGroup[] }`, newest first |
| `GET /v1/groups/:id` | `JobGroup` |
| `POST /v1/groups/:id/cancel` | cancels every unfinished shard |
| `POST /v1/groups/:id/retry` | 201; re-runs the shards that failed, were lost or were cancelled |

`JobGroup`: `id`, `name`, `owner`, `shardTotal`, `state` (`queued`, `running`, `succeeded`, `failed`,
`cancelled`), `jobs` (latest attempt of each shard, by shard index), `counts` (by job state), `nodes`,
`createdAt`, `startedAt`, `finishedAt`.

### Blobs and snapshots

| Method & path | Auth | Notes |
|---|---|---|
| `POST /v1/blobs/missing` | client or agent | `{ digests: ["sha256:…"] }` → `{ missing: [...] }` |
| `POST /v1/blobs/upload` | client or agent | `Content-Type: application/x-tar`; entries named `sha256/<hex>`; each verified → `{ stored, alreadyPresent, bytes }` |
| `POST /v1/blobs/download` | client or agent | `{ digests }` → tar stream of the same format |
| `GET /v1/blobs/:digest` | client or agent | raw bytes |
| `POST /v1/snapshots` | client | `{ manifestDigest }`; validates the manifest and that every blob exists → 201 `{ id, fileCount, totalBytes, createdAt, lastUsedAt, owner }` |
| `GET /v1/snapshots/:id` | client or agent | snapshot view |

A **manifest** is canonical JSON `{ "version": 1, "entries": [...] }` with entries sorted by path:
files `{ path, type: "file", size, digest, executable }` and symlinks
`{ path, type: "symlink", target }`. The snapshot ID is the SHA-256 of the manifest bytes.

### Tokens (admin)

| Method & path | Notes |
|---|---|
| `POST /v1/tokens` | `{ kind: "client"\|"join", name, role?, ttlSeconds?, maxUses? }` → 201 `{ token, info }`. The secret is returned only once. |
| `GET /v1/tokens` | `{ tokens: TokenView[] }` (never secrets) |
| `DELETE /v1/tokens/:id` | revoke; you cannot revoke the token making the request |

## WebSockets

### `GET /v1/events` (client token)

Client → controller:

```jsonc
{ "type": "subscribe", "topics": ["jobs", "nodes"] }
{ "type": "logs.subscribe", "jobId": "01M6…", "afterSeq": 0 }
{ "type": "logs.unsubscribe", "jobId": "01M6…" }
```

Controller → client: `job.updated {job}`, `group.updated {group}`, `node.updated {node}`,
`node.removed {nodeId}`,
`logs {jobId, entries}`, `logs.end {jobId, state}`, `error {message}`.

`logs.subscribe` first delivers every stored entry after `afterSeq`, then live entries. It ends with
`logs.end` once the job is terminal and all entries have been sent. To resume after a disconnect,
subscribe again with the last sequence number you saw; no entry is delivered twice.

### `GET /v1/agent/ws` (agent credential)

The agent channel is documented in [ARCHITECTURE.md §6.1](ARCHITECTURE.md#61-agent-channel-protocol)
and defined in `packages/protocol/src/agent-channel.ts`. It is versioned separately
(`AGENT_PROTOCOL_VERSION`); mismatched agents are refused with `PROTOCOL_MISMATCH`. Self-updating
agents then poll `GET /v1/agent/update` in `update-only` mode until the controller hands them its
build. The controller sends `{ type: "update.check" }` to ask a connected agent to call that
endpoint now.
