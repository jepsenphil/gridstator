# Installing Gridstator

This guide sets up a cluster from scratch on Linux machines (Windows machines join through WSL2;
see [WINDOWS.md](WINDOWS.md)). You need no knowledge of Kubernetes or distributed systems: one
machine runs the **controller**, every machine that should run jobs runs an **agent**, and your
editor or terminal is a **client**.

A typical home setup:

| Machine | Role |
|---|---|
| Laptop where your code lives | controller + agent (with CPU/RAM reserved for you) + client (CLI, VS Code) |
| Big desktop / server | agent |
| Windows PC | agent inside WSL2 |

## 1. Prerequisites (every machine)

- **Node.js 22.19 or newer** (`node -v`). Any installation method works (distribution package,
  [nodejs.org](https://nodejs.org), nvm).
- **Docker Engine** with buildx on machines that run jobs, usable without sudo:

  ```bash
  docker run --rm hello-world            # must work as your user
  sudo usermod -aG docker "$USER"        # if it does not; then log out and back in
  docker buildx version                  # for image builds
  ```

- **act** on machines that should run GitHub Actions workflows (optional):

  ```bash
  curl -fsSLO https://github.com/nektos/act/releases/latest/download/act_Linux_x86_64.tar.gz
  tar -xzf act_Linux_x86_64.tar.gz act && sudo install -m 0755 act /usr/local/bin/act
  ```

- **git** on the machine where your source code lives.

## 2. Install the `gridstator` command (every machine)

From a [GitHub release](https://github.com/jepsenphil/gridstator/releases):

```bash
npm install -g https://github.com/jepsenphil/gridstator/releases/download/v0.1.0/gridstator-0.1.0.tgz
gridstator --version
```

(Once published to npm: `npm install -g gridstator`.) The package is a single file with no runtime
dependencies. If `npm install -g` needs sudo on your system, either use a Node version manager or
install into your home: `npm install -g --prefix ~/.local <tarball>` with `~/.local/bin` on your PATH.

## 3. Create the cluster (controller machine)

```bash
gridstator controller init                                          # CA, database, admin token
gridstator controller install-service --with-agent --agent-name laptop   # runs at boot
gridstator nodes set laptop --reserve-cpus 4 --reserve-memory 16g --max-concurrency 2
gridstator doctor
```

`init` prints the cluster's **CA fingerprint** and saves this machine's client configuration
(`~/.config/gridstator/client.json`). `install-service` creates and starts a systemd user service
(`systemctl --user status gridstator-controller`) and enables lingering so it keeps running when
you log out. Leave out `--with-agent` if this machine should not run jobs.

The controller listens on TCP port 7300 on all interfaces. Allow it from your LAN if a firewall is
active, for example `sudo ufw allow from 192.168.1.0/24 to any port 7300`. Never forward it from your
router to the internet.

## 4. Add workers (each worker)

On the controller machine, create a join token (single use, valid 24 hours):

```bash
gridstator token create --join --name big-box
```

It prints the exact command to run on the worker:

```bash
gridstator agent join https://192.168.1.20:7300 --token gsj_… --fingerprint sha256:… --name big-box --labels big
sudo gridstator agent install-service --system      # or, without sudo: gridstator agent install-service
```

The system service runs as your user with access to Docker and starts at boot. Check it on the
controller with `gridstator nodes`. If `sudo` cannot find `gridstator` (for example with nvm, because
sudo resets PATH), run `sudo "$(command -v gridstator)" agent install-service --system`.

## 5. Connect other clients (optional)

Any machine can submit jobs once logged in. On the controller:

```bash
gridstator token create --name windows-pc
```

and run the printed `gridstator login https://… --fingerprint … --token …` on the client.

## 6. VS Code

Download `gridstator-<version>.vsix` from the release and install it with **Extensions: Install from
VSIX…** (or `code --install-extension gridstator-0.1.0.vsix`). With Remote SSH, install it in the
remote window, so it runs where your code is. It connects automatically using the CLI's shared
client configuration. All windows using the same remote account pick up credential changes
without reloading. On the controller host, the account owning its data automatically gets a shared
admin login; other clients can use **Gridstator: Sign In to Controller** once.

The dashboard's **Settings** tab shows controller/extension/worker versions and build identifiers,
editor preferences, tokens, retention and update policies. Admins can install official releases
(stable by default, prereleases opt-in) or upload a controller package. Published release checksums
are verified before installation. Updates wait for jobs to finish; cancelling jobs is explicit.

On the controller host, Settings can manage the systemd user service and change the listening
address/port with **Apply & Restart**. Supply a public HTTPS URL reachable by workers. The move
retains previous listeners for offline/older workers, verifies reconnection and recovers the
previous configuration if verification fails. An overlapping bind-address change requires a new
port so the old listener can remain available. A stopped service offers **Start Controller** and
is never started merely by opening VS Code. Service stop/restart waits for the embedded worker to
be idle; if it is busy, the action explains that its jobs must finish first.

Revoking the shared local login offers **Restore Local Access** rather than creating another token
automatically. `gridstator.autoConnect` can disable automatic connection. The extension ships its
own host helper, so it does not require an updated global CLI for local setup.

## 7. Try it

```bash
cd your-project
gridstator init                       # optional gridstator.yml with a sample task
gridstator run -- npm test
gridstator run --shards 2 -- node --test --test-shard={shard}/{total}
gridstator build -t my-app:dev
gridstator actions list
```

## Upgrading

Hand the controller the new build; workers installed with `install-service` follow it on their
own (see [Worker updates](#worker-updates)):

```bash
gridstator controller publish-build gridstator-<version>.tgz   # a release tarball (admin)
gridstator controller publish-build                            # or the build running this command
```

The controller installs the build into its version store, copies its database, and restarts on it
(its service must be a self-updating install, see below). The new version confirms itself once it is
serving. If it keeps crashing or does not confirm itself in time, the launcher returns to the
previous version and the controller puts back the copy of the database taken before the switch;
the newer database is kept as `controller.db.failed-<version>`. A development build works the same
way: run `pnpm bundle`, then `node apps/cli/release/dist/gridstator.mjs controller publish-build`.

Without a self-updating controller, install the new version (`npm install -g <new tarball>`) and
restart its service; it migrates its database automatically. Workers that are not self-updating
need the same version installed by hand and their service restarted.

### Worker updates

A worker is **current** when it runs the controller's exact build: the same version and the same
sha256 of the bundled entry point, so two dev builds with one version number are told apart.
`gridstator agents status` lists every worker as `current`, `behind`, `updating`, `failed`, or
`unknown` (offline; the last build it reported is shown). `gridstator status` and `gridstator
doctor` summarise the same, and VS Code shows it on each node.

```bash
gridstator agents status               # who runs what
gridstator agents status --check       # exit code 3 if a reachable worker is not current (CI, scripts)
gridstator agents update               # update every online worker that is behind (admin)
gridstator agents update big-box --now # cancel its running jobs instead of waiting for them
gridstator agents cancel               # stop a running rollout
```

Updates roll through the cluster one worker at a time. The controller stops placing jobs on the
worker, waits for running jobs to finish without a deadline, and tells the agent to download the
controller's build. Jobs are cancelled only when explicitly requested with `--now` or after an
explicit `--drain-timeout`. The agent
checks the download's sha256, installs it into its version store and restarts on it. The worker is
done when it reconnects on the new build. If the download fails, the new version keeps crashing,
or the worker does not come back within 15 minutes, the launcher returns to the previous version,
the worker is marked `failed`, and the rollout halts. Failed workers are not retried automatically;
fix the cause and run `gridstator agents update` again.

Auto-update decides when this happens without you:

| Mode | Behaviour |
|---|---|
| `follow` (default) | update as soon as a worker is behind |
| `window` | update only inside a daily window (controller's local time) |
| `off` | only on `gridstator agents update` |

```bash
gridstator agents set --auto-update window --window 02:00-05:00   # cluster default
gridstator agents set big-box --auto-update off                   # one worker
gridstator agents set big-box --auto-update inherit               # back to the default
```

#### act and Node.js

Self-updating workers also run the act and Node.js versions the controller's build pins (with their
sha256), and they count as `current` only when they do. The controller downloads each archive once,
checks it, and serves it to workers, so workers need no internet access; Node.js is switched by the
launcher (in place, no root needed) and is switched back if the agent fails on it. To pick other
versions or stop managing a tool:

```bash
gridstator agents set --act 0.2.96 --node pinned   # a version, "pinned" (the build's), or "off"
```

Workers can update only when the controller runs a bundled build (the release tarball, or
`pnpm bundle`), because that is what they download. Agents from before this feature, and agents not
installed with `install-service`, show as `behind` with what to do; install the new version on them
once. An agent whose agent-channel protocol no longer matches the controller's still updates itself
through a separate update endpoint that never changes.

### Self-updating installs

`install-service` copies the installed package into the service's own version store,
`<data dir>/install/versions/<version>-<hash>/` (owned by the service user, so no root is needed to
update), and the unit starts `<data dir>/install/launcher.mjs`. The launcher runs the active version
from `install/state.json`. If a new version fails to start three times, or does not report itself
healthy in time, the launcher goes back to the previous version.

Services installed before this existed run the global install in place. To convert one, install the
new tarball once and rerun `install-service` with the same options plus `--force`. Services
installed from the unbundled workspace build (`apps/cli/dist/main.js`) always run in place. Use the
release tarball, or `pnpm bundle` and `apps/cli/release`, to get a self-updating install.

### Lost the admin token

On the controller machine, as the user that owns its data directory:

```bash
gridstator controller token create --name admin-recovered --login
```

This writes a new admin token straight into the controller's database (it may be running) and logs
this machine in with it. Without `--login` it prints the token.

## Uninstalling

```bash
gridstator controller uninstall-service        # or: sudo gridstator agent uninstall-service --system
npm uninstall -g gridstator
rm -rf ~/.local/share/gridstator ~/.config/gridstator ~/.cache/gridstator   # data, credentials, caches
docker volume rm gridstator-cache                                            # package-manager cache on workers
```
