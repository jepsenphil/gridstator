# Gridstator native development preview

Historical preview: a later real Linux ARM workflow found the config-digest inspection gap described in PREVIEW.json. These bytes are retained for bounded evidence and are not the latest delivery.

Version `2.0.0-dev.1`, build `native-preview-20261110-cd6b769ebe6e`. These are four real native executables from the checkpointed, uncommitted working tree. The embedded source revision is the parent reference commit; `SOURCE_CHECKPOINT.json` and supplemental input records identify the actual build sources. This is not clean-commit or stable-release provenance.

This unpublished preview includes encoded blob-digest read-back, BuildKit containerd local-image metadata, and Darwin host-available memory corrections. The approved Darwin estimate uses free plus inactive pages, bounded to physical memory, and is deliberately distinct from Node os.freemem free-only semantics; Docker VM capacity stays independently bounded. Existing config digests retain priority; only local load/tar builds can fall back to the manifest/index digest when the config digest is absent. Earlier preview directories remain byte-for-byte unchanged as historical evidence. The native-only VSIX retains its verified original bytes.

The directory contains four platform archives, a release manifest, the native-only VSIX, the installer and bounded verification/provenance records. `gridstator-2.0.0-dev.1-native-bundle.tar.gz` contains only the manifest and four native archives. All adjacent regular files are covered recursively by `SHA256SUMS`.

```sh
sha256sum --check SHA256SUMS
# macOS alternative: shasum -a 256 --check SHA256SUMS
```

For a manual disposable check, select the archive matching the host OS and architecture. Extract only its regular root `gridstator` member into a private temporary directory, then run its `build-info --json` and `release verify` commands. The verifier takes the manifest SHA256, archive, extracted executable and target (`linux/amd64`, `linux/arm64`, `darwin/amd64` or `darwin/arm64`). Successful exact commands and results are retained in `verification/`.

The installer needs ordinary OS tools and curl. It downloads a CLI into a chosen prefix; initialization, joining and service installation are explicit later actions. No public download URL has been published. The private installer check used a prefix containing spaces, verified a repeated identical install, initialized disposable state, served authenticated pinned TLS status, uploaded a tiny native snapshot and returned its exact bytes through a `sha256%3A` blob URL. Those target processes had no Node, npm, Rust, Python or jq in PATH. The private HTTP/TLS orchestrator ran outside that restricted PATH. Owned HTTP/controller processes stopped and credentials/data were removed; no global prefix or live service changed.

`PREVIEW.json` and `provenance/` state the exact architecture, runtime and source scope of each check. A cross-build is not runtime proof. Mac Intel checks use Rosetta on M4, not physical Intel hardware. Mac bytes lack Developer ID signing and notarization; the ARM linker ad-hoc signature is not release signing. No minimum macOS compatibility is claimed beyond the recorded test host. Earlier broad proofs retain their original checkpoint scope.

This remains a development preview with the remaining release gates explicitly recorded. The native-only VSIX still requires VS Code's Node extension host; the controller and worker executables run natively. No release was published.

The exact final Linux ARM64 bytes also passed controller init/TLS/status/stop in an unprivileged, read-only, offline ARM64 Alpine container on the M4 Linux VM. This is actual ARM execution, with no ARM worker/systemd/boot claim. Unmodified build-time target metadata remains alongside separate later runtime records.
