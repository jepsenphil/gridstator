# Windows first-run setup

The twelve design decisions were confirmed before implementation. The Windows installer installs
the companion; dependency preparation happens in the companion's first-run wizard. CLI/headless
installation is unchanged.

## Selection and ownership

| Detected state | Default |
| --- | --- |
| Configured Gridstator worker | Preserve its distribution, account, node identity and settings; offer a check or repair |
| Working engine usable from the selected distribution | Prefer that engine; show its identity |
| No working engine, WSL2 already installed | Docker Engine inside Ubuntu |
| No working engine, WSL2 missing | Docker Desktop; WSL2 is still installed for the managed worker |

Both engine choices run the managed worker inside WSL2. New environments default to an owned
`Gridstator-Ubuntu` Ubuntu 24.04 LTS distribution and a non-root `gridstator` account. Linux
credential prompts are unnecessary. Existing compatible Ubuntu can be selected explicitly;
WSL1 is not converted, foreign distributions are not overwritten, and incompatible/conflicting
packages are reported for review rather than silently removed. A separate environment does not
migrate or replace an existing worker. Engine migration for a configured worker requires an
explicit operator procedure after stopping its work.

## Preparation and recovery

The review identifies the selected engine, distribution and required changes. Docker Desktop
installation requires acceptance of its agreement. The selected plan has a revision; stale
confirmations cannot authorize another plan. Windows elevation is limited to Microsoft's WSL
feature installation/update. Distribution registration, account provisioning and per-user Docker
Desktop installation run as the original Windows account.

WSL's installer obtains Ubuntu. Docker Desktop is downloaded from Docker and its Authenticode
signature and publisher are checked before execution. Docker Engine uses Docker's official
signed Ubuntu apt repository. Setup verifies systemd, a non-root account, Docker and Buildx,
Docker access from a user service, and a real container. Controller credentials are collected
only afterward, passed privately and never saved in the prerequisite journal. Enrollment finishes
only when the managed worker connects and reports Docker access.

The independent Windows host executes preparation while the GUI polls durable progress. Closing
the window leaves preparation running. A Windows restart is separately confirmed, uses a graceful
restart, and resumes the reviewed plan after sign-in. The journal retains the original engine
choice and records the Windows boot identity, so polling before a restart cannot restart
installation prematurely. A failed/interrupted installation retains its state and offers Retry
or an explicit engine change. It does not silently change engines. Firmware/admin/organization
restrictions produce an actionable blocked or failed state.

Docker Desktop configuration is preserved. For a reviewed Desktop setup, the Windows host starts
the runtime when its worker opts into start-at-sign-in, without changing Docker’s own startup
preference. Normal refresh and a manually stopped worker in the same sign-in session do not trigger it. When automatic WSL integration is unavailable, setup
names the exact distribution to enable in Docker Desktop and verifies access when the user returns.
Reused Linux sessions needing new Docker group membership require a guided session restart;
unrelated services and distributions are not stopped automatically.

Uninstall retains the owned distribution and all build data. The companion's separate deletion
option requires confirmation, validates the registration and ownership marker, and refuses running
workers/controllers. It deletes only the owned distribution and removes its local target entries.
Shared Docker installations, WSL2, user distributions and remote controller node records remain.

## Verification scope

Portable Rust tests exercise default precedence, plan revisions, concurrent requests, ownership,
failure recovery and reboot transitions through the real coordinator with an injected runner.
Browser tests render the production wizard with native transport fixtures; captures are UI evidence.
`windows/prerequisites.tests.ps1` tests actual production dispatch branches with mocked OS calls,
including graceful restart arguments and the installed-but-disabled WSL feature case. Windows CI
runs it without provisioning its host.

Read-only Windows checks cover PowerShell parsing, publisher signatures and detection of a real
Ubuntu/Docker Engine installation even when Docker Desktop is also installed. They do not establish
clean-machine installation, UAC under an alternate administrator account, actual reboot/sign-in
recovery, Docker Desktop first-install/integration, or destructive owned-distribution cleanup.
Those scenarios require disposable Windows qualification before promoting a release. A built NSIS
preview is unsigned development output; compilation and fixture tests do not close those gates.

Primary references: [WSL commands](https://learn.microsoft.com/en-us/windows/wsl/basic-commands),
[Ubuntu installation](https://documentation.ubuntu.com/wsl/latest/howto/install-ubuntu-wsl2/),
[Docker Desktop installation](https://docs.docker.com/desktop/setup/install/windows-install/),
[Docker WSL integration](https://docs.docker.com/desktop/features/wsl/),
[Docker Engine installation](https://docs.docker.com/engine/install/ubuntu/).
