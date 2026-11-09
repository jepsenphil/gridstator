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

> **Status: v0.1, early but complete.** Remote commands, working-tree snapshots, Docker builds,
> GitHub Actions via `act`, test sharding and the VS Code extension work and are tested, including
> across two physical machines (Linux and a Windows PC through WSL2). See [docs/PLAN.md](docs/PLAN.md).

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
- **Docker builds on another machine.** `gridstator build` runs BuildKit on a worker: target
  stages, build args, build secrets, tags, push, or export the image as a downloadable artifact. The
  build cache stays on the worker, and repeat builds go back to it.
- **GitHub Actions without pushing.** `gridstator actions run ci.yml` runs your existing workflows
  (or one job) with [act](https://github.com/nektos/act) on a worker, against your uncommitted
  files, with inputs, secrets, step-by-step progress and artifacts. `gridstator actions list` shows
  what can run locally and what cannot.
- **Parallel test shards.** `gridstator run --shards 4 -- npx vitest run --shard={shard}/{total}`
  spreads shards across workers, streams them side by side, and combines their JUnit results into
  one group; `gridstator group retry` re-runs only the failed shards.
- **VS Code extension.** A Gridstator Activity Bar with your nodes, jobs and everything runnable in
  the workspace; a dashboard with live logs, steps, artifacts, cancel/retry/rerun and node
  management; errors in the Problems panel and JUnit results in the Testing view. Works over
  Remote SSH, running where your code lives.
- **Secure by default.** TLS everywhere with a self-generated cluster CA pinned by fingerprint,
  named and revocable tokens, and no inbound ports on workers. See [docs/SECURITY.md](docs/SECURITY.md).

## Requirements

- Node.js 22.19 or newer on every machine.
- Docker on every machine that runs jobs, usable by the user running the agent (buildx for image
  builds).
- [act](https://github.com/nektos/act) and git on workers that should run GitHub Actions workflows.
- git on the machine where your source code lives (recommended).
- VS Code 1.105 or newer for the extension.

## Quick start

Install on each machine (from a [release](https://github.com/jepsenphil/gridstator/releases); the
full walkthrough, including services that start at boot, is in [docs/INSTALL.md](docs/INSTALL.md)):

```bash
npm install -g https://github.com/jepsenphil/gridstator/releases/download/v0.1.0/gridstator-0.1.0.tgz
```

**1. Create the cluster** on the machine that will run the controller (it can also be a worker):

```bash
gridstator controller init
gridstator controller install-service --with-agent   # controller + a worker here, started at boot
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
sudo gridstator agent install-service --system       # or: gridstator agent start
```

Windows PCs join through WSL2; see [docs/WINDOWS.md](docs/WINDOWS.md).

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

**5. Build images and run workflows:**

```bash
gridstator build --target runner -t web:dev            # BuildKit on a worker
gridstator build --output tar && gridstator artifacts <job-id> --download image.tar
gridstator actions list
gridstator actions run ci.yml --job test
gridstator actions run deploy.yml -e workflow_dispatch -i environment=staging
```

## VS Code

Install the extension from `gridstator.vsix` (**Extensions: Install from VSIX…**, or
`code --install-extension gridstator.vsix`; build it with `pnpm package:vscode`). In a Remote SSH
window, install it on the remote side, where your code is. Then run **Gridstator: Sign In to
Controller** (or `gridstator login` in that machine's terminal; both share the same configuration).

Open the Gridstator Activity Bar view and press ▶ next to a task, workflow job, package script or
Dockerfile, or open the dashboard (**Gridstator: Open Dashboard**) for logs, steps, artifacts and node
management. **Gridstator: Select Target Worker** chooses a worker for this workspace (default:
automatic). Jobs run the files saved on disk; the extension asks first if editors have unsaved
changes.

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
    command: corepack enable && pnpm install --frozen-lockfile && pnpm vitest run --reporter=junit --outputFile=junit.xml
    junit: junit.xml           # results appear in VS Code's Testing view
builds:
  web: { file: Dockerfile, stage: runner, tags: [web:dev] }
actions:
  platforms: { ubuntu-latest: catthehacker/ubuntu:act-latest }
  secretEnv: [GITHUB_TOKEN]    # forwarded from your environment when set
```

Run a task with `gridstator run --task test` and a build with `gridstator build -b web`. Package-manager caches (npm, pnpm, yarn, pip, Go)
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
| `build [context]` | build a Docker image on a worker |
| `actions list` / `actions run <workflow>` | discover and run GitHub Actions workflows with act |
| `artifacts <job> [--download <name>]` | list or download a job's artifacts |
| `run --shards N`, `groups`, `group show/cancel/retry` | sharded runs and their combined results |
| `controller install-service`, `agent install-service` | run as systemd services that start at boot |
| `snapshot [--dry-run] [--json]` | see exactly which files would be sent |
| `token create/list/revoke` | manage client and join tokens |
| `init` | create `gridstator.yml` |

Most commands accept `--json`. Exit codes: the job's exit code for `run`, `130` if cancelled,
`75` if lost; `2` usage error, `3` controller unreachable, `4` authentication, `5` not found.

## Documentation

- [Installation](docs/INSTALL.md), [Windows / WSL2](docs/WINDOWS.md), [Troubleshooting](docs/TROUBLESHOOTING.md)
- [Architecture](docs/ARCHITECTURE.md): components, flows, data model, decisions
- [Security](docs/SECURITY.md): threat model and controls
- [API](docs/API.md): controller REST and WebSocket API
- [Development](docs/DEVELOPMENT.md): building, testing, multi-machine validation
- [Plan](docs/PLAN.md): milestones and progress
- [Changelog](CHANGELOG.md), [Contributing](CONTRIBUTING.md), [Code of conduct](CODE_OF_CONDUCT.md), [Security policy](SECURITY.md)

## License

Apache-2.0. See [LICENSE](LICENSE).
