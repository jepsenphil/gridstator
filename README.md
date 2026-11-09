# Gridstator

[![CI](https://github.com/jepsenphil/gridstator/actions/workflows/ci.yml/badge.svg)](https://github.com/jepsenphil/gridstator/actions/workflows/ci.yml)

**Multiple machines. One workflow.**

Gridstator turns the computers on your local network into a development compute cluster. Run your
tests, builds and other commands on whichever machine has capacity, against the exact files in
your working tree (including uncommitted changes) without pushing to GitHub or copying code around.

```console
$ gridstator run -- pnpm test
Snapshot of /home/me/web-app: 412 files, 3.1 MiB (git, 2 hashed, 41ms)
Job 01M6YRS9QKK0X6GDE3YDMNB8NB submitted (snapshot a7966da95eff); waiting for scheduler
[gridstator] Job 01M6YRS9QKK0X6GDE3YDMNB8NB (attempt 1) accepted by node big-box
[gridstator] Snapshot a7966da95eff: 412 entries, 3.1 MiB, fetching 2 of 410 blobs
...your test output, streamed live...
Job 01M6YRS9QKK0X6GDE3YDMNB8NB succeeded on big-box in 41s
```

> **Status: early development (v0.1).** The distributed execution core and workspace snapshots
> (milestones 1 and 2) work and are tested. Docker image builds, GitHub Actions via `act`, the
> VS Code extension and test sharding are next. See [docs/PLAN.md](docs/PLAN.md).

## What works today

- **Controller + agents.** One controller holds the queue and history; lightweight agents on each
  machine run jobs in Docker. Any machine can host the controller, alone or alongside an agent.
- **Run commands remotely.** `gridstator run -- <command>` runs in a fresh container on a suitable
  node, streams stdout/stderr live, and exits with the job's exit code.
- **Exact working-tree snapshots.** Tracked files, untracked files and uncommitted edits are
  captured (respecting `.gitignore`), content-addressed, and only changed files are transferred.
  Secrets such as `.env` and private keys are excluded by default, with a warning.
- **Scheduling.** Capability, label and resource-aware placement; per-node concurrency limits and
  CPU/RAM reserved for the host (so your development laptop stays responsive); priorities;
  enable/disable/drain; pinning a job to a node.
- **Reliable lifecycle.** Cancel, retry, automatic retry of lost jobs, and persistent history with
  an audit trail. Jobs survive controller restarts and agent reconnects without losing log lines.
- **Secure by default.** TLS everywhere with a self-generated cluster CA pinned by fingerprint,
  named and revocable tokens, and no inbound ports on workers. See [docs/SECURITY.md](docs/SECURITY.md).

## Requirements

- Node.js 22.19 or newer on every machine.
- Docker on every machine that runs jobs, usable by the user running the agent.
- git on the machine where your source code lives (recommended).

## Quick start

Install on each machine (from a release tarball until the package is published to npm):

```bash
npm install -g ./gridstator-0.1.0.tgz
```

**1. Create the cluster** on the machine that will run the controller (it can also be a worker):

```bash
gridstator controller init
gridstator controller start --with-agent      # controller + a worker on this machine
```

`init` prints the CA fingerprint and an admin token, and saves them as this machine's client
configuration.

**2. Add a worker.** On the controller machine:

```bash
gridstator token create --join
```

Run the printed command on the worker, then start the agent:

```bash
gridstator agent join https://192.168.1.20:7300 --token gsj_... --fingerprint sha256:...
gridstator agent start
```

**3. Connect your development machine** (if it is not the controller):

```bash
gridstator token create --name my-laptop        # on the controller machine
gridstator login https://192.168.1.20:7300 --fingerprint sha256:... --token gsc_...
```

**4. Run something:**

```bash
cd my-project
gridstator nodes
gridstator run -- npm test
gridstator run --node big-box --cpus 8 --memory 16g -- pnpm build
gridstator run --detach -- ./scripts/long-task.sh
gridstator jobs
gridstator logs <job-id> --follow
gridstator cancel <job-id>
gridstator retry <job-id>
```

Press Ctrl-C once during `gridstator run` to cancel the remote job; press it again to detach and
leave the job running.

## Keeping your laptop usable

Every node reserves CPU and memory for its host. Jobs are only placed where their requested
resources fit in what remains, and never when the node's currently available memory is too low:

```bash
gridstator nodes set laptop --reserve-cpus 6 --reserve-memory 24g --max-concurrency 1
gridstator nodes drain laptop        # finish running jobs, accept no new ones
gridstator nodes disable laptop      # accept no new jobs
gridstator nodes enable laptop
```

## Project configuration

`gridstator init` writes an optional `gridstator.yml`:

```yaml
version: 1
defaults:
  image: node:24-bookworm-slim
  resources: { cpus: 2, memory: 4g }
snapshot:
  include: []        # paths git ignores that should still be sent
  exclude: []        # extra paths to leave out
  allowSecrets: []   # secret-looking files that are safe to send (e.g. .env.test)
tasks:
  test:
    command: corepack enable && pnpm install --frozen-lockfile && pnpm test
```

Run a task with `gridstator run --task test`. Package-manager caches (npm, pnpm, yarn, pip, Go)
persist on each worker in a shared Docker volume, so repeat installs are fast.

## Command reference

| Command | Purpose |
|---|---|
| `controller init` / `controller start [--with-agent]` | create and run the controller |
| `agent join <url>` / `agent start` | register and run a worker |
| `login <url>` | connect a client machine |
| `status`, `doctor` | cluster overview; diagnose setup problems |
| `nodes`, `nodes show/set/enable/disable/drain/remove` | inspect and manage workers |
| `run`, `jobs`, `job`, `logs`, `cancel`, `retry` | submit and manage jobs |
| `snapshot [--dry-run] [--json]` | see exactly which files would be sent |
| `token create/list/revoke` | manage client and join tokens |
| `init` | create `gridstator.yml` |

Most commands accept `--json`. Exit codes: the job's exit code for `run`, `130` if cancelled,
`75` if lost; `2` usage error, `3` controller unreachable, `4` authentication, `5` not found.

## Documentation

- [Architecture](docs/ARCHITECTURE.md): components, flows, data model, decisions
- [Security](docs/SECURITY.md): threat model and controls
- [API](docs/API.md): controller REST and WebSocket API
- [Development](docs/DEVELOPMENT.md): building, testing, multi-machine validation
- [Plan](docs/PLAN.md): milestones and progress

## License

Apache-2.0. See [LICENSE](LICENSE).
