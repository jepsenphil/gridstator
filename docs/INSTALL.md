# Install native Gridstator

**Status:** native 2.0.0 is a development preview and the planned first public release. This guide describes the verified install path after the [release gates](RELEASING.md) pass. For current testing, [build from source](DEVELOPMENT.md).

## Requirements

- Linux amd64/arm64 or macOS Intel/Apple Silicon for the native CLI. On Windows, run the worker inside a supported WSL2 distribution; see [Windows](WINDOWS.md).
- Docker available to the account that runs each worker. Buildx is needed for image builds; Git and `act` are needed for GitHub Actions jobs. The CLI/controller host need not run Docker unless it is also a worker.
- VS Code 1.105+ for the extension. The native CLI must be installed on the **workspace host**, including a Remote SSH or WSL host.

The installed native CLI has no Node, npm or Rust requirement. VS Code supplies the extension's Node runtime.

## Get and verify a release

Choose a published v2 tag only after its [release record](RELEASING.md) links all required acceptance evidence. The release contains platform archives, `gridstator-release.json`, `SHA256SUMS` and `install-native.sh`. Download the installer and checksum document from the **same tag** and verify the installer before executing it. The installer then verifies the manifest, matching archive, executable and architecture.

~~~sh
tag=v2.0.0                         # replace with an actual published tag
base=https://github.com/jepsenphil/gridstator/releases/download
stage=$(mktemp -d)
curl -fsSL -o "$stage/SHA256SUMS" "$base/$tag/SHA256SUMS"
curl -fsSL -o "$stage/install-native.sh" "$base/$tag/install-native.sh"
awk '$2 == "install-native.sh" { print; count++ } END { if (count != 1) exit 1 }' \
  "$stage/SHA256SUMS" > "$stage/installer.sha256"
(cd "$stage" && sha256sum --check installer.sha256)
# On macOS, use: (cd "$stage" && shasum -a 256 --check installer.sha256)
/bin/sh "$stage/install-native.sh" --tag "$tag" --prefix "$HOME/.local"
"$HOME/.local/bin/gridstator" build-info --json
~~~

The installer does not initialize a cluster or start a service. For manual archive verification, exact package layout and rollback, follow [the native release procedure](../native/RELEASING.md). Never run an installer for a tag that has not been published and reviewed.

## Start a fresh cluster

On the controller host, choose a reachable LAN address instead of `127.0.0.1` when workers connect from other machines:

~~~sh
gridstator controller init --host 0.0.0.0 --port 7300
gridstator controller install-service --print --with-agent
gridstator controller install-service --with-agent
gridstator status
~~~

`init` creates a new CA, database and admin login. Review the printed service before installation. Omit `--with-agent` if the controller machine should not run jobs. A user service starts at account sign-in; use an explicitly reviewed system service only when boot-time operation is required. Use a private firewall rule for the controller port on the trusted network.

On the controller host, create a one-use join command:

~~~sh
gridstator token create --join --name worker-one --uses 1
~~~

Run the displayed `gridstator agent join ...` command on the intended worker. Then:

~~~sh
gridstator agent install-service --print
gridstator agent install-service
gridstator doctor
gridstator nodes
~~~

Treat the join command and token as credentials. A worker dials the controller; it needs no inbound worker port. For a client on another machine, create a client token on the controller and run the displayed `gridstator login ...` command on the source host.

## VS Code

The first public extension will use the Marketplace ID `gridstator.gridstator`. Until it is published, build the [native VSIX](../apps/vscode/README.md) from source and install it with **Extensions: Install from VSIX**. Install the extension and native CLI on the remote workspace host for Remote SSH/WSL. Open the Gridstator view, set up a local controller or connect using the native login, and use **Run** to launch a task or workflow.
