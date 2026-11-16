# Windows workers and WSL2

Windows can run a native `gridstator.exe` worker built from source. WSL2 remains an
alternative using the released Linux binary. The desktop companion and Windows-local
VS Code setup still use their existing WSL integration; native Windows worker support
here is through the CLI.

## Native Windows worker

Build on Windows x64 or ARM64 with the pinned Rust toolchain and Visual Studio C++
build tools. Run these commands in PowerShell from the repository root:

```powershell
Set-Location native
cargo build --locked --release
$gridstator = (Resolve-Path .\target\release\gridstator.exe).Path
& $gridstator build-info --json
```

Start Docker Desktop with **Linux containers**, and check `docker info` and
`docker buildx version` from the same Windows account. Docker's VM CPU and memory
limits determine execution capacity; leave room for the Windows host. Git is needed
for Actions jobs. The controller can supply managed `act.exe`; an existing installation
on PATH or an explicit `GRIDSTATOR_ACT` path also works.

On the controller, issue a one-use worker token:

```sh
gridstator token create --join --name windows-worker --uses 1
```

Run its displayed `agent join` arguments with the Windows executable, using the
controller's reachable URL and pinned fingerprint. Then start the worker in the
foreground:

```powershell
& $gridstator agent join https://controller-host:7300 --token JOIN_TOKEN --fingerprint CA_FINGERPRINT --name windows-worker
& $gridstator agent start
```

Use the actual token and fingerprint from the controller. Keep the foreground process
running; Ctrl+C stops it. Worker state defaults to the Windows account's local
application-data directory under `gridstator-v2\agent`. Set `GRIDSTATOR_AGENT_DATA`
consistently in each terminal to use another directory. The worker connects outbound
and needs no inbound worker port.

Native Windows supports Docker command jobs, BuildKit image builds and supported
Linux-container Actions workflows. Command snapshots and BuildKit tar contexts retain
Unix executable modes. Actions workflows restore executable modes inside their Linux
workspace. Host-staged snapshots must use names Windows can represent: case collisions,
reserved devices, alternate data streams and ambiguous trailing dots/spaces are rejected.
Staging snapshot symlinks requires Windows Developer Mode or symlink privileges.

Local CLI worker controls use same-account named pipes. Windows service installation,
start-at-sign-in and managed native supervisor updates are not available in this path;
`agent install-service` is for Linux/systemd or macOS/launchd. There is no published
native Windows release claim until actual Windows runtime and Docker workload evidence
has been collected.

## Worker inside WSL2

1. Install an Ubuntu WSL2 distribution with systemd enabled. Ensure Docker Engine in that distribution, or Docker Desktop's WSL integration, is usable by the WSL account that will run Gridstator.
2. Install the **Linux archive matching the WSL architecture** from [INSTALL.md](INSTALL.md), inside WSL. For development, build the native CLI there from source.
3. On the controller, issue a one-use join token. In WSL, run the displayed `gridstator agent join ...` command with the controller URL and pinned CA fingerprint.
4. Inspect `gridstator agent install-service --print` and install the user service. Run `gridstator doctor` and `gridstator nodes` to verify Docker, connectivity and admission capacity.

A WSL worker dials the controller, so it needs no incoming worker port. WSL memory and CPU limits can be lower than the Windows host's; check Docker's actual capacity and leave a host reservation. A systemd user service and WSL sign-in lifecycle must be validated for the machine where it runs.

## VS Code and desktop

The Windows desktop installer installs the companion. Its first-run wizard can prepare WSL2,
a dedicated Ubuntu LTS environment and Docker, with review, restart recovery and preservation of
existing workers. Docker Desktop is an alternative engine for the same managed WSL2 worker.
See [Windows first-run setup](WINDOWS_SETUP.md) for defaults, recovery and verification scope.

Install the native extension and CLI on the **workspace side** of a Remote WSL window.
Opening a Windows-local VS Code window against a Windows folder does not make the Linux
worker binary available on that host. The [desktop companion](../apps/desktop/README.md)
remains a separate development preview using WSL worker controls.

## Validation

[Native CI](../.github/workflows/native.yml) includes Windows x64 and ARM64 build jobs,
ACL/named-pipe controls, process-tree cancellation, snapshot staging, packaging and a
native controller/worker registration smoke test. These checks do not require Docker.
Run them locally from `native/`:

```powershell
cargo test --locked --lib windows -- --test-threads=1
cargo test --locked --lib local_transport_controls_policy_and_refuses_duplicate_listener -- --test-threads=1
cargo test --locked --test native_worker_platform --test packaging_contract -- --test-threads=1
```

Real Docker workloads require a running Linux-container daemon. Run the ignored
[executor contract suite](../native/TESTING.md) on the Windows account used for workers:

```powershell
cargo test --locked --test executor_contract -- --ignored --test-threads=1
```

Actual WSL2 qualification is tracked separately in the [native progress report](NATIVE_PROGRESS.md)
and [validation host guide](../native/VALIDATION_HOSTS.md).
