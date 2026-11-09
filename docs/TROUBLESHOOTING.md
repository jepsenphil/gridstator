# Troubleshooting

Start with `gridstator doctor`: it checks Node.js, git, the client configuration, the controller
connection (including the pinned certificate), your token, every node's health and the project
configuration, and prints a hint for each problem.

Logs:

| Component | Where |
|---|---|
| Controller (service) | `journalctl --user -u gridstator-controller -f` (or `sudo journalctl -u …` for a system service) |
| Agent (system service) | `sudo journalctl -u gridstator-agent -f` |
| A job | `gridstator logs <job> [--follow]`, or the dashboard |
| VS Code extension | Output panel → **Gridstator** |

## Connecting

**`Cannot reach the controller at https://…:7300 (ECONNREFUSED / EHOSTUNREACH)`**
The controller is not running or not reachable. On the controller: `systemctl --user status
gridstator-controller`. From the client: check the address, and that port 7300 is allowed by the
controller machine's firewall. On WSL in NAT mode, other machines cannot reach a controller inside
WSL; see [WINDOWS.md](WINDOWS.md).

**`… presented a certificate that is not signed by the pinned cluster CA`** or **`CA fingerprint …
does not match`**
The client trusts a different cluster than the one answering, usually because the controller was
re-initialized (`controller init --force`). Log in again with the new fingerprint
(`gridstator login …`) and re-join agents (`gridstator agent join … --force`). If you did not
re-initialize anything, something may be intercepting the connection.

**`Invalid or revoked token`** — the token was revoked or the cluster re-created. Create a new one
with `gridstator token create` on the controller and log in again.

**`Join token … has already been used`** — join tokens are single-use by default. Create a new one
(`gridstator token create --join`, or `--uses 3` for several workers).

## Workers

**Node shows `offline`** — the agent is not running or cannot reach the controller. Check its
service and logs on the worker. For WSL workers, WSL may have stopped (see
[WINDOWS.md](WINDOWS.md#5-keep-wsl-running)). Jobs that were running there become `lost` after a
minute and are retried if their retry policy allows.

**Node `degraded`: `Docker unavailable: permission denied …/docker.sock`** — the agent's user cannot
use Docker. Add it to the `docker` group (`sudo usermod -aG docker <user>`) and restart the agent; a
shell started before the change still lacks the group (log out and in). System services created
with `install-service --system` get the group automatically.

**`act` or `buildx` missing in `gridstator nodes`** — install them on that worker and restart its
agent; capabilities are detected at agent start.

**`A node named "x" is already registered`** when joining — pick another `--name`, or remove the old
node first (`gridstator nodes remove x`).

**`Invalid update request: drainTimeoutSeconds: … expected number, received null`** or
**`This controller cannot wait indefinitely …`** — the client is newer than the running
controller. Update the controller first with
`gridstator controller publish-build <current-package.tgz>`, then retry **Update workers**. For a
workspace build, prepare the package with `pnpm pack:release`. Rebuilding the extension alone does
not update the controller service. Older controllers cancel remaining jobs after one hour if the
deadline is omitted, so omitting it does not preserve the default behavior of waiting for jobs.

## Jobs

**A job stays `queued`** — `gridstator job <id>` (or the dashboard) shows why:
- `waiting for 8 CPU, 16 GiB…` — no node has that much free right now; lower `--cpus/--memory` or
  wait.
- `job needs … but node x allows at most …` — the request exceeds what any node offers after its
  reservation; lower the request or the node's reservation (`gridstator nodes set`).
- `lacks label(s) gpu` — no node has the label; check `--label` / `labels:` and the node labels.
- `node x is draining / disabled` — enable it (`gridstator nodes enable x`).

**A file is missing in the job** — `gridstator snapshot --dry-run --json` lists exactly what is sent.
Git-ignored files, `node_modules`, `.git` and secret-looking files (`.env`, keys, `.npmrc` with tokens,
`.secrets`) are left out; the warnings say which. Use `snapshot.include` or `snapshot.allowSecrets`
in `gridstator.yml` to send them anyway.

**`File … kept changing while the snapshot was captured`** — something (a watcher, a dev server) is
writing to it continuously. Stop it, or exclude the file in `gridstator.yml`.

**`npm ci` / `pnpm install` is slow every time** — package-manager caches live in the worker's
`gridstator-cache` Docker volume and warm up after the first run on each worker. Jobs from the same
project prefer the worker that ran them last.

**Out-of-memory (`killed because it ran out of memory`)** — raise `--memory` (the container limit).

## Docker builds

**`Dockerfile "x" does not exist in the snapshot`** — paths are relative to the current directory
on the command line, and to the project root in `gridstator.yml`. Check that the file is not
git-ignored.

**Push fails** — the worker's Docker must be logged in to the registry (`docker login` as the
agent's user on that worker).

## GitHub Actions (act)

**Very long first run** — the default runner image (`catthehacker/ubuntu:act-latest`) is large; it is
pulled once per worker. Map `ubuntu-latest` to a smaller image in `gridstator.yml` (`actions.platforms`)
if your workflow allows.

**Actions fail to download / API rate limit** — act clones actions from GitHub. Forward a token:
`GITHUB_TOKEN=… gridstator actions run ci.yml --secret-env GITHUB_TOKEN`.

**`docker: command not found` / cannot connect to Docker inside a workflow** — the Docker socket is
not mounted by default. Use `--docker-socket` (or `actions.dockerSocket: true`) for workflows that
build images; see [SECURITY.md](SECURITY.md) for what that allows.

**A nested Docker container cannot see files created by a workflow step** — Docker-enabled
workflows share the worker's staged snapshot directory with the runner. Update older Gridstator
workers if a nested `docker run -v "$PWD:…"` sees a separate copy or cannot write its report.
Start a new job after changing `gridstator.yml`; **Retry** preserves the original snapshot and
job settings.

**`gridstator actions list` says a job cannot run** — act runs Linux containers only; Windows and
macOS runners are not supported locally.

## VS Code

**No Gridstator icon** — reload the window (**Developer: Reload Window**). With Remote SSH, make sure
the extension is installed in the remote window (Extensions view → *Install in SSH: …*).

**"Not connected"** — the extension reads the CLI's client configuration on the machine where your
code is. Run `gridstator doctor` in that machine's terminal, or **Gridstator: Sign In to
Controller**.

**Secrets from `gridstator.yml` are "not set in VS Code's environment"** — the extension sees the
environment of the VS Code (server) process, not of your interactive shell. Run such jobs from the
CLI, or set the variables where VS Code is started.

## Starting over

```bash
gridstator controller init --force     # new cluster; the old data directory is kept as a backup
```

Then log in clients and re-join agents with the new fingerprint.
