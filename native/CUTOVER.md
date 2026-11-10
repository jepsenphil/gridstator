# Native installation, cutover and recovery

Use this runbook with a selected native release that has passed its recorded release gates. The
native core has no Node/npm/Rust/compiler requirement on the target machine. Docker/buildx, act
and git remain worker prerequisites; VS Code supplies the extension's Node runtime. A preview
rehearsal does not pass the outstanding Mac, WSL2, mixed-target or signing gates.

The native controller starts a **fresh cluster**. Its CA, credentials, node IDs and job history are
separate from v1. Workers rejoin; v1 history stays in an archive. There is no database conversion or
rolling v1/native cluster upgrade. Keep the exact old v1 installation and its Node runtime available
until the fresh cluster has been accepted. Returning to v1 restores that old cluster separately;
it does not import jobs created in v2.

## Before stopping the old cluster

1. Record the v1 CLI version, immutable package/release path and checksum, Node version/path,
   controller URL, CA fingerprint, cluster ID, service account, and loaded controller/agent service
   definitions. Save the effective `DOCKER_HOST`, `DOCKER_CONTEXT`, `DOCKER_CONFIG` and Docker context.
   Preserve the exact existing installation, or retain its original verified distribution and
   required Node runtime so recovery does not depend on an unpublished working tree.
2. Record every controller/agent data directory and client config override. The usual Linux v1
   paths are `~/.local/share/gridstator/{controller,agent}` and
   `~/.config/gridstator/client.json`; `GRIDSTATOR_*` and XDG overrides take precedence.
3. Drain v1 workers using the v1 CLI. Confirm active jobs have finished, durable results have
   reached the controller, and history/logs/artifacts needed for recovery are present. If forced
   cancellation is required, record which jobs were cancelled before proceeding.
4. Stop the **recorded v1 units**, and verify their processes have exited. Do not stop units by
   guessing names or matching executable names. Preserve their definitions; disable them if they
   would otherwise restart alongside v2 after reboot.
5. Archive the stopped controller directory, worker directories and client credentials together
   with the recorded service definitions. Keep CA/private keys, DB/WAL files, logs, snapshots,
   artifacts and updater state. Give the archive directory mode 0700 and archives mode 0600.
   Hash the archives and test listing/extraction into an empty private directory. Do not archive a
   live SQLite database with a plain file copy.

For example, after replacing the paths with the recorded stopped v1 paths:

```sh
umask 077
archive_root="$HOME/gridstator-v1-archive-2026-11-10"
mkdir -m 700 "$archive_root"
tar -czf "$archive_root/controller.tar.gz" -C "$HOME/.local/share/gridstator" controller
tar -czf "$archive_root/client.tar.gz" -C "$HOME/.config/gridstator" client.json
sha256sum "$archive_root/controller.tar.gz" "$archive_root/client.tar.gz" > "$archive_root/SHA256SUMS"
# On macOS use shasum -a 256 instead of sha256sum.
tar -tzf "$archive_root/controller.tar.gz"
```

Keep the original v1 data directories intact as an additional recovery copy. Use absolute v2
paths below and clear stale environment overrides instead of pointing the native program at v1
state. Native startup refuses a non-v2 controller directory.

## Verified small installer

Choose an existing reviewed native tag; the value below is an example, not a published-release
promise. Download the installer and its checksum document from that exact tag, verify the installer
before execution, then let it verify the selected manifest/archive/executable. The issuer is the
configured HTTPS release origin; checksums detect damaged or mismatched downloaded bytes.

```sh
umask 077
tag=v2.0.0-preview.1
base=https://github.com/jepsenphil/gridstator/releases/download
stage=$(mktemp -d)
curl --fail --location --proto '=https' --proto-redir '=https' -o "$stage/SHA256SUMS" "$base/$tag/SHA256SUMS"
curl --fail --location --proto '=https' --proto-redir '=https' -o "$stage/install-native.sh" "$base/$tag/install-native.sh"
awk '$2 == "install-native.sh" { print; count++ } END { if (count != 1) exit 1 }' "$stage/SHA256SUMS" > "$stage/installer.sha256"
(cd "$stage" && sha256sum --check installer.sha256)
# On macOS: (cd "$stage" && shasum -a 256 --check installer.sha256)
/bin/sh "$stage/install-native.sh" --tag "$tag" --prefix "$HOME/.local"
"$HOME/.local/bin/gridstator" build-info --json
rm -rf "$stage"
```

The installer selects Linux/WSL2 or macOS amd64/arm64, verifies the exact archive and manifest
checksums, rejects duplicate/nonregular executable entries and wrong executable architecture, and
invokes the native release verifier before an atomic binary replacement. It preserves verified
metadata under `<prefix>/share/gridstator/releases/<tag>/`. It does not initialize a cluster or
install/start a service. Reuse of a tag with different retained manifest bytes is rejected.

## Manual download alternative

Download `SHA256SUMS`, `gridstator-release.json` and the exact matching
`gridstator-<version>-<os>-<arch>.tar.gz` into a private empty staging directory. Verify the manifest
and archive against their unique entries in `SHA256SUMS` before extracting. Select `linux-amd64`,
`linux-arm64`, `darwin-amd64` or `darwin-arm64` from the actual machine. Do not execute another
architecture's binary or treat successful cross-compilation as a runtime test.

For example, for a Linux amd64 download (adjust `tag` and `platform` for the selected release):

```sh
umask 077
tag=v2.0.0-preview.1
platform=linux-amd64
target=linux/amd64
base=https://github.com/jepsenphil/gridstator/releases/download
prefix="$HOME/.local"
stage=$(mktemp -d)
archive="gridstator-${tag#v}-$platform.tar.gz"
for name in SHA256SUMS gridstator-release.json "$archive"; do
  curl --fail --location --proto '=https' --proto-redir '=https' -o "$stage/$name" "$base/$tag/$name"
done
awk -v archive="$archive" '$2 == archive || $2 == "gridstator-release.json" { print; count++ } END { if (count != 2) exit 1 }' "$stage/SHA256SUMS" > "$stage/selected.sha256"
(cd "$stage" && sha256sum --check selected.sha256)
# On macOS use shasum -a 256 --check selected.sha256.
tar -tzf "$stage/$archive"
# The expected package contains exactly regular gridstator and LICENSE members.
# Inspect the executable type and reject links/duplicates; do not unpack arbitrary paths.
tar -tvzf "$stage/$archive" gridstator
tar -xOzf "$stage/$archive" gridstator > "$stage/gridstator"
chmod 700 "$stage/gridstator"
manifest_sha=$(awk '$2 == "gridstator-release.json" { print $1 }' "$stage/selected.sha256")
"$stage/gridstator" release verify --manifest "$stage/gridstator-release.json" \
  --manifest-sha256 "$manifest_sha" --archive "$stage/$archive" \
  --executable "$stage/gridstator" --target "$target"
mkdir -p "$prefix/bin" "$prefix/share/gridstator/releases/$tag"
cp "$stage/gridstator-release.json" "$stage/SHA256SUMS" "$prefix/share/gridstator/releases/$tag/"
# Stage on the destination filesystem so replacement is atomic.
install_stage=$(mktemp "$prefix/bin/.gridstator.XXXXXXXX")
cp "$stage/gridstator" "$install_stage"
chmod 755 "$install_stage"
mv -f "$install_stage" "$prefix/bin/gridstator"
rm -rf "$stage"
```

Prefer the installer for the automated duplicate/link/architecture checks. Manual operators must
perform those inspections before executing the downloaded binary. Keep the verified manifest in
the prefix layout above: the explicit service installer adopts the CLI into its managed version
store using that matching manifest. On macOS verify the downloaded executable's Developer ID and
Gatekeeper assessment on the actual Mac; source crosschecks do not replace that gate.

## Initialize and rejoin the fresh native cluster

Use a new config/data path and a new native service name. The default native service names are
`gridstator-native-controller` and `gridstator-native-agent` on Linux, with corresponding
`org.gridstator.native.*` launchd labels. They are separate from v1 service names.

```sh
native_cli="$HOME/.local/bin/gridstator"
export GRIDSTATOR_CONFIG="$HOME/.config/gridstator-v2/client.json"
export GRIDSTATOR_CONTROLLER_DATA="$HOME/.local/share/gridstator-v2/controller"
export GRIDSTATOR_AGENT_DATA="$HOME/.local/share/gridstator-v2/agent"
"$native_cli" controller init --host 0.0.0.0 --port 7300
"$native_cli" controller install-service --print
# Review the generated account, data directory, PATH and Docker connection.
"$native_cli" controller install-service
"$native_cli" status --json
"$native_cli" nodes --json
```

Record the new CA fingerprint and cluster ID. Confirm they differ from v1 and that old credentials
are rejected. The service installer is explicit; merely downloading a binary never starts a role.
Use `--system --user <account>` only when a system service was deliberately selected. Linux user
services require an available user service manager; boot/logout behavior depends on the chosen user
service policy. Mac user LaunchAgents start after login and may remain connected while Docker
Desktop is not ready. Confirm host and Docker VM capacities separately on Mac.

Issue new join credentials with the native CLI, then run `agent join` on each worker using the new
controller URL and displayed fingerprint. Treat the token as a credential; do not paste it into
shared logs. Reusing a friendly node name does not reuse its old v1 identity.

```sh
"$native_cli" token create --join --name fresh-native-worker --uses 1 --json
# On the intended worker, using that newly issued token and the new CA fingerprint:
"$native_cli" agent join https://controller.example:7300 \
  --token '<new-join-token>' --fingerprint 'sha256:<new-ca-fingerprint>' --name worker-one
"$native_cli" agent install-service --print
"$native_cli" agent install-service
```

Check `doctor`, online workers, expected Docker context, reservations, an exact working-tree
command, cancellation, logs and artifacts before admitting ordinary work. Reconfigure the VS Code
extension explicitly: enable native mode, select the native executable, and connect using the new
config/cluster. Keep v1 history available separately. Record clean-target install, service/restart,
Docker-late-start and signing evidence per actual OS/architecture.

## Native update and rollback checks

Use the verified native **bundle**, not the old npm tarball:

```sh
"$native_cli" controller publish-build /absolute/path/gridstator-2.0.0-preview.2-native-bundle.tar.gz
"$native_cli" status --json
"$native_cli" agents status --json
```

The controller mirrors all target archives, stages its target, drains an embedded worker, creates
a pre-switch SQLite backup and durable switch marker, and activates the candidate provisionally.
The supervisor starts the confirmation deadline when launching the payload. A healthy candidate
confirms after its listener passes a pinned health check. Repeated crash/timeout returns to the
previous executable; startup recovery restores the pre-candidate DB before opening it, and keeps
the failed DB as `controller.db.failed-<build-id>-<id>` for inspection. Verify job history, schema,
CA and cluster identity after recovery. Do not merge a failed candidate database into the restored
one or delete the retained DB until diagnosis is complete.

Keep the bundle, manifest/checksums, executable build-info, supervisor logs, install `state.json`,
pre-switch DB backup and failed-DB facts as evidence. Do not manually rewrite `state.json` to
force a candidate current or clear pending confirmation. Recover credentials, when necessary,
with `controller token create` using the locally owned v2 data directory; use the matching v2 CLI.

## Separately restore v1

If the fresh native cluster is not accepted, drain and stop only its recorded native roles. Preserve
its v2 data for diagnosis. Restore the stopped v1 archives into empty original/account-owned paths,
verify archive hashes first, and use the exact retained v1 installation and Node runtime. Restore
only its recorded service definitions, start the old controller, and verify the old CA fingerprint,
cluster ID, credentials and historical jobs before bringing old workers back. Do not point v1 at
v2 data or native credentials. Reconnect clients/VS Code to the restored v1 config explicitly.

## Repeat the disposable Linux rehearsal

From `native/` (so `rust-toolchain.toml` selects the pinned toolchain), with the musl target/C
compiler and the already built v1 reference CLI available:

```sh
GRIDSTATOR_KEEP_RECOVERY_EVIDENCE=1 \
  cargo test --test recovery_e2e -- --ignored --nocapture
```

Set `GRIDSTATOR_RECOVERY_SYSTEMD=1` to run the native role under a uniquely named disposable user
unit. This requires a running user systemd manager; the test removes only its own unit. The
foreground supervisor path is the default and needs no service manager.

`GRIDSTATOR_REHEARSAL_CC` may name an absolute musl-capable compiler wrapper;
`GRIDSTATOR_V1_CLI` may select the exact retained v1 `main.js`. An optional private
`GRIDSTATOR_RECOVERY_BUILD_CACHE` reuses compiler outputs across rehearsals; it contains no cluster
data. To repeat the execution phase using a previous rehearsal's three exact payloads and recorded
source/hash manifests, set `GRIDSTATOR_RECOVERY_PREBUILT` to that retained evidence directory; the
report identifies this reuse and its original payload source directory. Build-host Rust/C tools and the old
v1 Node runtime are used only for rehearsal construction/recovery. The installer subprocess gets a
private PATH containing ordinary OS tools and no Node/npm/Rust/compiler.

The test builds three actual static Linux executables in a disposable source copy. It verifies the
small installer and manual path, archives/restores an isolated v1 history, creates a fresh native
identity, publishes a healthy native candidate and confirms it, then publishes an actual executable
that adds a SQLite column/table, changes `user_version`, deletes job rows and exits before health.
It requires rollback to the previous executable, original schema/history, and separately retained
failed candidate DB. Its other-target bundle entries are header-only parsing fixtures, so this
rehearsal does **not** pass ARM/Mac runtime, mixed-target LAN rollout, Gatekeeper, reboot or WSL2
acceptance. Those require their own actual-target evidence.
