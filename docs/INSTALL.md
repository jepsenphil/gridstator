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
remote window, so it runs where your code is. It uses the same client configuration as the CLI;
otherwise run **Gridstator: Sign In to Controller**.

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

Install the new version on every machine (`npm install -g <new tarball>`) and restart the services
(`systemctl --user restart gridstator-controller`, `sudo systemctl restart gridstator-agent`).
The controller migrates its database automatically. Run the same version everywhere: an agent
whose protocol version differs from the controller's is refused with a message saying so.

## Uninstalling

```bash
gridstator controller uninstall-service        # or: sudo gridstator agent uninstall-service --system
npm uninstall -g gridstator
rm -rf ~/.local/share/gridstator ~/.config/gridstator ~/.cache/gridstator   # data, credentials, caches
docker volume rm gridstator-cache                                            # package-manager cache on workers
```
