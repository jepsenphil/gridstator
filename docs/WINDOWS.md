# Windows and WSL2

The native CLI/controller/worker targets Linux and macOS. On Windows, the first v2 worker path runs the **Linux native binary inside WSL2**. The [desktop companion](../apps/desktop/README.md) has a native Windows window and WSL worker controls, but remains a separate development preview with outstanding OS qualification.

## Worker inside WSL2

1. Install an Ubuntu WSL2 distribution with systemd enabled. Ensure Docker Engine in that distribution, or Docker Desktop's WSL integration, is usable by the WSL account that will run Gridstator.
2. After a verified v2 release exists, install the **Linux archive matching the WSL architecture** from [INSTALL.md](INSTALL.md), inside WSL. For development, build the native CLI there from source.
3. On the controller, issue a one-use join token. In WSL, run the displayed `gridstator agent join ...` command with the controller URL and pinned CA fingerprint.
4. Inspect `gridstator agent install-service --print` and install the user service. Run `gridstator doctor` and `gridstator nodes` to verify Docker, connectivity and admission capacity.

A WSL worker dials the controller, so it needs no incoming worker port. WSL memory and CPU limits can be lower than the Windows host's; check Docker's actual capacity and leave a host reservation. A systemd user service and WSL sign-in lifecycle must be validated for the machine where it runs.

## VS Code

Install the native extension and CLI on the **workspace side** of a Remote WSL window. Opening a Windows-local VS Code window against a Windows folder does not make the Linux worker binary available on that host. For local Windows worker controls, use the separate desktop companion preview.

## Migrating from v1

The v1 Windows/WSL Node instructions are [archived](legacy/v1/WINDOWS.md). Native v2 uses new data, credentials and service names. Follow the [cutover and recovery runbook](../native/CUTOVER.md) before changing a working v1 cluster. Actual WSL2 release qualification is tracked in the [v2 progress report](V2_PROGRESS.md).
