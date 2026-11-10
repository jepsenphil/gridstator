# Actual-host native validation

Use the M4 MacBook Pro for `darwin/arm64` worker, Docker Desktop and launchd validation. Keep the
Ubuntu x64 machine as a disposable native controller and Linux worker. Windows 11 supplies the
separate WSL2 validation host. These instructions create new state; they do not migrate an old
cluster or establish that a preview has passed the stable release gates.

No native Darwin release is published at the time this runbook was written. Obtain a reviewed CI
candidate rather than substituting an imagined release download URL. Target machines need the
native executable and ordinary installation tools, without Node/npm/Rust. Docker/buildx, git and
act remain external worker tools. The VS Code extension runs in VS Code's supplied Node runtime.

## Build and acquire the M4 candidate

The [candidate workflow](../.github/workflows/native-candidate.yml) builds each executable on its
corresponding host: `ubuntu-24.04`, `ubuntu-24.04-arm`, `macos-15-intel` and `macos-15`.
GitHub documents the latter two as Intel and ARM64 respectively. These builds, unit tests and
linkage checks provide different evidence from a Docker/launchd/reboot run on your laptop.
GitHub's ARM64 Mac runners do not support nested virtualization, so do the Docker Desktop proof on
the M4. See [GitHub's runner reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners).

After the reviewed source has been pushed to an owner-selected branch, run **Native release
candidate** from GitHub Actions with a unique prerelease version, for example
`2.0.0-preview.1`. For an unsigned development preview, leave `notarize-preview` false. Stable
versions always require Developer ID signing/notarization and the credentials in
[RELEASING.md](RELEASING.md); that input cannot waive the stable gate.

An owner can use the GitHub UI or these optional build-host commands. Replace the branch, version
and run ID with the actual reviewed run; this is an instruction to run later, not an existing run:

```sh
reviewed_branch=replace-with-reviewed-branch
gh workflow run native-candidate.yml --repo jepsenphil/gridstator \
  --ref "$reviewed_branch" -f version=2.0.0-preview.1 -f notarize-preview=false
gh run list --repo jepsenphil/gridstator --workflow native-candidate.yml
run_id=replace-with-actual-numeric-run-id
gh run view "$run_id" --repo jepsenphil/gridstator --json url,headSha,conclusion
```

Wait for the **assemble** job to pass. On the M4, download its `native-release-candidate` artifact
through that run's GitHub UI and unzip it into a new private directory. The equivalent optional
GitHub CLI download is:

```sh
umask 077
validation_root=$(mktemp -d "$HOME/gridstator-validation.XXXXXXXX")
mkdir "$validation_root/release"
gh run download "$run_id" --repo jepsenphil/gridstator \
  --name native-release-candidate --dir "$validation_root/release"
```

Record the run URL, exact head SHA, version and build ID. Download the assembled artifact, which
contains the manifest and checksums, rather than only the individual `candidate-darwin-arm64`
executable. Use the same assembled release on the Ubuntu controller and other validation workers.
The Linux artifacts can be built in the agreed Docker build environment; Darwin artifacts come
from the Mac CI jobs and require the Apple toolchain to link.

## Verify and manually install the preview

Start a native ARM64 terminal on the M4. `uname -m` must say `arm64`; a terminal running under
Rosetta is a separate translated test. Rust distinguishes `aarch64-apple-darwin` from
`x86_64-apple-darwin`, and documents Rosetta execution separately in its
[Darwin target notes](https://doc.rust-lang.org/rustc/platform-support/apple-darwin.html).

The following assumes `validation_root` is the private directory above, with the assembled files
directly inside its `release` subdirectory. Adjust `version` to the actual workflow input. Stop on
any failed check. The archive must list exactly two regular files, `gridstator` and `LICENSE`;
reject a link, repeated name or any unexpected member before extracting the executable.

```sh
version=2.0.0-preview.1
archive="gridstator-$version-darwin-arm64.tar.gz"
prefix="$validation_root/prefix"
uname -m
sw_vers
(cd "$validation_root/release" && shasum -a 256 --check SHA256SUMS)
tar -tzf "$validation_root/release/$archive"
tar -tvzf "$validation_root/release/$archive"
mkdir -p "$validation_root/stage" "$prefix/bin"
tar -xOzf "$validation_root/release/$archive" gridstator > "$validation_root/stage/gridstator"
chmod 700 "$validation_root/stage/gridstator"
file "$validation_root/stage/gridstator"
lipo -archs "$validation_root/stage/gridstator"
```

The `file`/`lipo` results must identify ARM64 Mach-O. `lipo` and the later `otool` linkage inspection
use Apple's command-line developer tools; they are validation tools, not native program runtime
dependencies. For a signed candidate, record
`codesign --verify --strict --verbose=2` and `spctl --assess --type execute --verbose=4` against this
downloaded executable, with the accepted notarization receipt from CI. An unsigned preview cannot
pass that gate. If macOS blocks a reviewed unsigned preview, any per-app exception is an explicit
owner action following [Apple's opening guidance](https://support.apple.com/en-us/102445). Do not
disable Gatekeeper globally or count an exception as signing/notarization evidence.

After deciding to run the verified preview, check its embedded identity and use its native
manifest/archive/executable verifier before installing it:

```sh
manifest_sha=$(awk '$2 == "gridstator-release.json" { print $1; count++ } END { if (count != 1) exit 1 }' "$validation_root/release/SHA256SUMS")
"$validation_root/stage/gridstator" build-info --json
"$validation_root/stage/gridstator" release verify \
  --manifest "$validation_root/release/gridstator-release.json" \
  --manifest-sha256 "$manifest_sha" --archive "$validation_root/release/$archive" \
  --executable "$validation_root/stage/gridstator" --target darwin/arm64
mkdir -p "$prefix/share/gridstator/releases/v$version"
cp "$validation_root/release/gridstator-release.json" "$validation_root/release/SHA256SUMS" \
  "$prefix/share/gridstator/releases/v$version/"
install_stage=$(mktemp "$prefix/bin/.gridstator.XXXXXXXX")
cp "$validation_root/stage/gridstator" "$install_stage"
chmod 755 "$install_stage"
mv -f "$install_stage" "$prefix/bin/gridstator"
native_cli="$prefix/bin/gridstator"
"$native_cli" build-info --json > "$validation_root/build-info.json"
otool -L "$native_cli" > "$validation_root/linkage.txt"
```

The reported target must be `darwin/arm64`, with the expected version/build ID. Keeping the
manifest under that prefix allows the explicit service installer to adopt this executable into
the verified managed version store. The release's small installer is an alternative once there is
an actual selected HTTPS release origin; its commands and checksum verification are in
[CUTOVER.md](CUTOVER.md). This CI artifact procedure needs no download server or target Node runtime.

## Prepare Docker and measure its execution pool

Install/start Docker Desktop for Apple Silicon in the login account that will run the worker.
Record Docker Desktop's selected VM backend and **Settings → Resources → Advanced** CPU and memory
limits. Choose limits appropriate to the laptop's actual memory and other work; do not allocate
the whole machine by assumption. Docker documents these as VM resources and offers an optional
start-at-login setting in its [settings reference](https://docs.docker.com/desktop/settings-and-maintenance/settings/).

```sh
sysctl -n hw.ncpu
sysctl -n hw.memsize
docker context show
docker version
docker info --format 'server={{.OSType}}/{{.Architecture}} cpus={{.NCPU}} memoryBytes={{.MemTotal}}'
docker buildx version
docker run --rm --platform linux/arm64 alpine:3.21 uname -m
git --version
```

Require a reachable Linux Docker engine, working buildx, and `aarch64` from the native ARM64
container. For Actions validation, provision act through the controller's managed toolchain (or
record an explicitly selected external act installation). Keep the Docker context/config and any
`DOCKER_HOST` override consistent between the interactive shell and service account.

Native node `info.cpus`/`memoryTotalMb` and host metrics describe macOS. The separate
`info.executionCapacity` describes Docker's CPU/RAM limit. The scheduler uses the smaller capacity
and subtracts the configured reservations. For example, a measured 4-CPU/8-GiB Docker VM with
1 CPU/2 GiB reserved should offer about 3 CPUs/6 GiB, even on a larger Mac. Reservations do not
resize Docker's VM. Current VM free-memory/disk measurements are unavailable and are reported as
null; host available memory and agent-data host disk must not be presented as Docker VM free space.

Keep the initial concurrency small (the join example uses one) and verify the controller's
`nodes show --json` output against both measurements before scheduling memory-heavy work. A worker
can remain connected while Docker is unavailable; it must not advertise runnable executors until
Docker is ready.

## Attach to a disposable native controller

On Ubuntu, install the **same verified candidate's** `linux/amd64` archive using
[CUTOVER.md](CUTOVER.md). Use a new private directory, a free validation port and an address that
the M4 can reach. These commands start a foreground controller without an embedded worker:

```sh
umask 077
controller_root=$(mktemp -d "$HOME/gridstator-controller-validation.XXXXXXXX")
export GRIDSTATOR_CONFIG="$controller_root/client.json"
export GRIDSTATOR_CONTROLLER_DATA="$controller_root/controller"
native_cli=/absolute/path/to/verified/prefix/bin/gridstator
"$native_cli" controller init --host 0.0.0.0 --port 17300
"$native_cli" controller start --data-dir "$GRIDSTATOR_CONTROLLER_DATA"
```

Keep that terminal running. In a second Ubuntu shell with the same `GRIDSTATOR_CONFIG`, issue a
one-use join token and a short-lived client token, and save the new cluster ID/CA fingerprint:

```sh
"$native_cli" status --json
"$native_cli" token create --join --name m4-validation --uses 1 --json
"$native_cli" token create --name m4-validation-client --role admin --ttl 4h --json
```

Use the Ubuntu machine's actual LAN address/DNS name in the controller URL and its displayed
fingerprint. Keep tokens out of shared evidence. On the M4, set isolated state, log in with the
new client token, then join with the separate new join token:

```sh
export GRIDSTATOR_CONFIG="$validation_root/client.json"
export GRIDSTATOR_AGENT_DATA="$validation_root/agent"
export GRIDSTATOR_INSTALL_ROOT="$validation_root/install"
controller_url=https://replace-with-ubuntu-lan-address:17300
"$native_cli" login "$controller_url" \
  --token '<new-client-token>' --fingerprint 'sha256:<new-ca-fingerprint>'
"$native_cli" agent join "$controller_url" \
  --token '<new-join-token>' --fingerprint 'sha256:<new-ca-fingerprint>' \
  --name m4-validation --data-dir "$GRIDSTATOR_AGENT_DATA" \
  --max-concurrency 1 --reserve-cpus 1 --reserve-memory 2g
"$native_cli" agent start --data-dir "$GRIDSTATOR_AGENT_DATA"
```

Keep the worker in the foreground first. From a second Mac shell with the same client config,
record `doctor --json`, `nodes show m4-validation --json`, and run an explicit ARM64 Docker smoke
job against that node from a private empty project directory:

```sh
mkdir "$validation_root/project"
cd "$validation_root/project"
printf 'native-m4-snapshot\n' > marker.txt
"$native_cli" run --node m4-validation --image alpine:3.21 --cpus 1 --memory 256m \
  --name m4-snapshot-smoke --artifact proof.txt -- \
  'test "$(uname -m)" = aarch64 && cat marker.txt && cp marker.txt proof.txt'
"$native_cli" jobs --json
```

Require exit 0, expected snapshot contents/logs and downloadable `proof.txt`. Then exercise a
nonzero exit, cancellation, a Docker build and an Actions workflow. For a late-Docker test, stop
Docker Desktop after jobs finish, confirm the worker stays connected with no runnable executors,
start Docker again and require a fresh successful job. Do not count an amd64 container emulated
inside Docker as a native Intel Mac or Linux amd64 worker test.

## Validate the login service separately

Stop the foreground worker gracefully before starting its service. The user service is a
LaunchAgent in `~/Library/LaunchAgents/org.gridstator.native.agent.plist`, loaded in
`gui/<uid>`. It starts after that user logs in. The explicit `--system --user <account>` mode is a
LaunchDaemon in `/Library/LaunchDaemons`, loaded in the system domain and running as that named
account. System mode does not make a login-dependent Docker Desktop engine available before login.

The CLI uses a fixed native label. Only install it in a validation account without an existing
native agent service; otherwise keep the foreground proof or use a separate macOS account.
Check both the loaded label and plist before installing. Do not use `--force` to replace another
installation.

```sh
launchctl print "gui/$(id -u)/org.gridstator.native.agent"
ls "$HOME/Library/LaunchAgents/org.gridstator.native.agent.plist"
# Both should be absent in this validation account before proceeding.
"$native_cli" agent install-service --data-dir "$GRIDSTATOR_AGENT_DATA" --print \
  > "$validation_root/agent.plist"
plutil -lint "$validation_root/agent.plist"
# Review absolute executable/store/data paths, HOME, PATH and Docker environment.
"$native_cli" agent install-service --data-dir "$GRIDSTATOR_AGENT_DATA"
launchctl print "gui/$(id -u)/org.gridstator.native.agent"
```

Require the same node identity online, the expected managed build ID, an actual successful Docker
job and clean stop/restart. Test logout/login, reboot/login, Docker starting late, provisional
update confirmation and failed-update rollback on this host. Keep the service logs under
`$GRIDSTATOR_AGENT_DATA/service-logs` and sanitized controller observations. When this rehearsal
ends, drain the worker, run `agent uninstall-service` from this validation account, verify its
label/plist are gone, and stop only the disposable controller. Preserve the private evidence before
removing validation state. These are actions to perform on the actual Mac, not gates already passed.

## What the available hosts can establish

| Host | Useful evidence | Separate evidence still needed |
| --- | --- | --- |
| M4 macOS, ARM64 binary and Linux ARM64 Docker containers | Native Apple Silicon roles, Docker VM capacity, launchd/login, late Docker, updates/rollback, actual VS Code | Actual Intel Mac behavior; stable downloaded signed/notarized artifact |
| M4 running the Intel Mac binary with Rosetta | Additional translated `darwin/amd64` compatibility probe | Native Intel hardware validation; Rosetta is not that proof |
| Ubuntu x64 host | Linux amd64 Docker work, controller, systemd and recovery | ARM64, Mac and WSL2 behavior |
| Ubuntu ARM64 VM virtualized on the M4 | Actual `linux/arm64` userspace/kernel, ARM64 Docker and systemd in a VM | Mac launchd and physical ARM Linux boot/power behavior |
| Windows 11 with an actual WSL2 Ubuntu distro | Linux native helper, Docker connection, systemd/session behavior and VS Code Remote WSL | Windows-native executable support, which is outside this release |

For the optional ARM Linux VM, use UTM's **Virtualize → Linux** with an Ubuntu ARM64 image, give it
explicit CPU/RAM/disk, install Docker Engine inside Ubuntu and confirm `uname -m` is `aarch64`.
This follows the [UTM Ubuntu guide](https://docs.getutm.app/guides/ubuntu/); choosing x86 emulation
would be a different test. Preserve the VM version/config with its evidence.

On Windows 11, follow [Microsoft's WSL install instructions](https://learn.microsoft.com/en-us/windows/wsl/install),
then record `wsl --version` and `wsl --list --verbose` in PowerShell and require version **2** for
the selected Ubuntu distro. Inside that distro, verify `uname -m`, the actual Docker engine/context,
systemd availability and the matching Linux binary. Run the helper in the WSL workspace host and
use the native extension setting there. A Docker Linux container on Windows or an Ubuntu VM on
the M4 does not establish WSL2 behavior.

For each host keep: run/source/build identity, manifest/archive/executable hashes, OS/architecture,
service scope/account, Docker/buildx/act versions and connection, host/VM/reservation measurements,
commands and exit codes, job IDs/results, sanitized logs, restart/login/reboot observations and
update/rollback facts. Record unsupported or unrun cases explicitly. Stable publication requires
the outstanding [parity ledger](contracts/parity-ledger.json) gates and review, including signed
downloaded Mac artifacts on both actual Mac architectures.
