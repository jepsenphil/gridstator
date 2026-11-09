# Developing Gridstator

## Prerequisites

- Node.js 22.19+ (CI and development use Node 24)
- pnpm 10 (`corepack enable` provides it)
- Docker with BuildKit/buildx, usable without sudo (needed for executor and end-to-end tests)
- git

## Setup and everyday commands

```bash
pnpm install
pnpm build            # tsc -b across all packages (project references)
pnpm test             # unit + protocol tests (Docker executor tests run if Docker is available)
pnpm test:e2e         # real controller + 2 agents + Docker, via the built CLI
pnpm typecheck        # sources and tests
pnpm lint             # ESLint (type-aware)
pnpm format           # Prettier
pnpm check            # typecheck + lint + format check + unit tests
pnpm pack:release     # single-file bundle + npm tarball in apps/cli/release/
```

Run the CLI from source with `pnpm gridstator <args>` (after `pnpm build`).

## Repository layout

| Path | Contents |
|---|---|
| `packages/shared` | errors (codes, HTTP status, exit codes), ids and tokens, logger, paths, size/duration parsing, fingerprints |
| `packages/protocol` | zod schemas: job spec, states and transitions, nodes, manifests, REST bodies, WebSocket messages, `gridstator.yml` |
| `packages/scheduler` | pure `schedule()` function |
| `packages/snapshot` | project discovery, capture, secret filter, blob store, tar streams, materialization, upload |
| `packages/executors` | `docker` CLI wrapper, `Executor` interface, container command executor |
| `packages/client` | HTTPS/WSS client with CA pinning, client configuration |
| `apps/controller` | PKI, SQLite store, auth, cluster state machine and scheduler loop, Fastify server, retention |
| `apps/agent` | agent connection loop, job runner, log pipeline, metrics, join |
| `apps/cli` | `gridstator` command; `scripts/bundle.mjs` builds the publishable package |
| `tests/integration` | end-to-end tests that spawn real processes |
| `examples/` | sample projects for manual validation |

Dependencies point downward only: apps → client/executors/snapshot/scheduler → protocol → shared.

## Testing strategy

| Layer | Where | What it proves |
|---|---|---|
| Pure logic | `packages/scheduler`, `packages/protocol` | placement rules, transitions |
| Snapshots | `packages/snapshot/src/snapshot.test.ts` | git file selection, secrets, determinism, CAS integrity, tar safety, materialization safety |
| Executor | `packages/executors/src/container.test.ts` | real Docker: output, exit codes, env/secrets, workspace copy, cancel, timeout, cleanup |
| Controller protocol | `apps/controller/src/controller.test.ts` | real HTTPS/WSS controller with scripted fake agents: auth, lifecycle, idempotency, cancellation, retries, lost nodes, restart reconciliation, blob verification |
| End to end | `tests/integration/e2e.test.ts` | built CLI processes, two agents, Docker: remote runs, uncommitted snapshots, secret masking, concurrency, labels, cancel/retry, controller restart, agent crash |

Running two agents on one machine exercises the same code paths as two physical machines (each
has its own data directory, node identity and Docker labels). The remaining difference, real LAN
networking, is covered by the manual procedure below.

## Manual multi-machine validation

This verifies the Milestone 1 and 2 acceptance criteria across two machines: **A** (where your
source lives, runs the controller) and **B** (a worker). Replace `192.168.1.20` with A's LAN
address.

1. **Install on both machines.** On A: `pnpm pack:release`, then copy
   `apps/cli/release/gridstator-0.1.0.tgz` to B. On each: `npm install -g ./gridstator-0.1.0.tgz`.
   B needs Docker usable by your user (`docker run --rm hello-world`).

2. **Create the cluster on A:**

   ```bash
   gridstator controller init
   gridstator controller start           # leave running; add --with-agent to also use A
   ```

   Allow TCP 7300 from your LAN in A's firewall if one is active (e.g. `sudo ufw allow from 192.168.1.0/24 to any port 7300`).

3. **Join B.** On A run `gridstator token create --join`, then on B run the printed command:

   ```bash
   gridstator agent join https://192.168.1.20:7300 --token gsj_... --fingerprint sha256:...
   gridstator agent start
   ```

   Check on A: `gridstator nodes` shows B `online`.

4. **Remote execution with live logs (M1):**

   ```bash
   gridstator run --no-snapshot --node <B> --image alpine:3.20 -- \
     sh -c 'for i in $(seq 1 10); do echo "tick $i on $GRIDSTATOR_NODE_NAME"; sleep 1; done'
   ```

   Expect one `tick` line per second, naming B.

5. **Cancellation (M1):** start `gridstator run --no-snapshot --node <B> --image alpine:3.20 -- sleep 600`,
   press Ctrl-C once. Expect `Job … cancelled` within a few seconds, and on B
   `docker ps -a --filter label=io.gridstator.managed=true` lists no job container.

6. **Retry and history (M1):** `gridstator retry <id>`, then `gridstator jobs` and `gridstator job <id>`.

7. **Uncommitted changes (M2):**

   ```bash
   cp -r examples/node-project /tmp/gst-demo && cd /tmp/gst-demo
   git init -q && git add -A && git commit -qm init
   gridstator run --node <B> --task test                 # passes
   sed -i 's/add(2, 3), 5/add(2, 3), 6/' test/math.test.js # do not commit
   gridstator run --node <B> --task test                 # fails: B ran the edited test
   gridstator snapshot --dry-run --json | grep -c '"path"'
   ```

8. **Resilience:** while a long job runs on B, restart the controller on A; the job continues and
   `gridstator logs <id>` shows every line. Kill the agent on B with `kill -9` and restart it;
   the job becomes `lost` (and is retried if submitted with `--retries 1`).

## Windows and WSL2

The controller, agent and CLI are plain Node.js and use only cross-platform APIs. For a Windows
machine to contribute compute, run the agent **inside WSL2** with Docker Engine installed in the
WSL distribution (or Docker Desktop's WSL integration enabled), exactly as on Linux. This was
validated on Ubuntu 24.04 under WSL2 with systemd enabled (see [PLAN.md](PLAN.md#two-machine-validation-2026-11-09)).
Notes:

- The agent only makes outbound connections, so WSL's NAT/mirrored networking and the Windows
  firewall need no changes on the worker.
- WSL2 gets half of the host's RAM by default. To offer more to jobs, set `memory=` under `[wsl2]`
  in `%UserProfile%\.wslconfig` and run `wsl --shutdown`.
- After `usermod -aG docker $USER`, start a new login session before starting the agent.
- To run the agent in the background, use a systemd service, for example (until `install-service`
  arrives in M7):

  ```bash
  sudo systemd-run --unit=gridstator-agent --uid=$USER --gid=$USER -p SupplementaryGroups=docker \
    /usr/local/bin/gridstator agent start
  ```

Fully validated Windows guidance (services, WSL settings) is part of Milestone 7.

## Releasing

`pnpm pack:release` produces `apps/cli/release/gridstator-<version>.tgz`: a single bundled ESM file
with no runtime dependencies. Release automation and npm publishing arrive with Milestone 7.
