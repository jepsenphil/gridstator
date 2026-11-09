# Windows machines (WSL2)

A Windows PC contributes compute by running the Gridstator agent inside **WSL2**, where Docker
runs Linux containers natively. This setup was validated on Ubuntu 24.04 under WSL2 with systemd
enabled and mirrored networking. The agent only makes outbound connections, so no Windows firewall
rules are needed for a worker.

## 1. WSL2 with systemd

In PowerShell (as administrator) install Ubuntu if you have not already:

```powershell
wsl --install -d Ubuntu-24.04
```

Inside Ubuntu, make sure systemd is enabled (current WSL versions enable it by default):

```bash
cat /etc/wsl.conf          # should contain:  [boot]  systemd=true
systemctl is-system-running
```

If you change `/etc/wsl.conf`, run `wsl --shutdown` in PowerShell and open Ubuntu again.

## 2. Give WSL enough memory

WSL2 uses half of the Windows RAM by default. To let jobs use more, create
`%UserProfile%\.wslconfig`:

```ini
[wsl2]
memory=48GB
processors=16
# Optional: use the Windows machine's LAN address directly
networkingMode=mirrored
```

then run `wsl --shutdown` and reopen Ubuntu. `gridstator nodes show <name>` shows what the agent sees.

## 3. Docker Engine inside WSL

Install Docker Engine in the Ubuntu distribution (Docker's apt repository; see
<https://docs.docker.com/engine/install/ubuntu/>), then:

```bash
sudo usermod -aG docker "$USER"     # then close and reopen the Ubuntu window
docker run --rm hello-world
```

Docker Desktop with WSL integration also works; the agent uses whichever `docker` the WSL user can run.

## 4. Node.js, Gridstator and the agent

Follow [INSTALL.md](INSTALL.md) steps 2 and 4 inside Ubuntu:

```bash
gridstator agent join https://<controller>:7300 --token gsj_… --fingerprint sha256:… --name windows-pc
sudo gridstator agent install-service --system
```

## 5. Keep WSL running

WSL stops a distribution some time after its last window or process ends, which takes the agent
offline. Options:

- Keep an Ubuntu terminal open while the PC should be a worker.
- Or start WSL at sign-in with a scheduled task: Task Scheduler → *Create Task* → trigger *At log
  on* → action `wsl.exe` with arguments `-d Ubuntu-24.04 --exec sleep infinity`.

The controller marks the node offline when WSL stops; queued work waits or goes to other nodes, and
the node comes back online automatically when WSL starts again.

## Running the controller on Windows

Prefer a Linux machine (or the laptop where your code lives) for the controller. If it must run in
WSL, use `networkingMode=mirrored` so other machines can reach port 7300, and allow the port in
the Windows firewall (`New-NetFirewallRule -DisplayName Gridstator -Direction Inbound -Protocol TCP
-LocalPort 7300 -Action Allow -RemoteAddress LocalSubnet`).

## VS Code on Windows with Remote SSH

The common setup: VS Code runs on Windows and connects to a Linux machine with Remote SSH. Install
the Gridstator extension **in the remote window** (it then runs on the Linux side, next to your code
and your `gridstator login`). Nothing needs to be installed on Windows itself for this.
