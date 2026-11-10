# Gridstator native development preview

Version `2.0.0-dev.1`, build `native-preview-20261110-6994d2cc804b`. These are four real native executables from the checkpointed, uncommitted working tree. The embedded source revision is the parent reference commit. `SOURCE_CHECKPOINT.json` and supplemental input records identify the actual native build sources; this is not clean-commit or stable-release provenance.

This preview includes encoded blob-digest read-back, inspectable local BuildKit image references and the approved Darwin host-available memory estimate. Local load/tar builds prefer a working config reference, then try the distinct manifest/index reference. Existing config success, zero-byte images and remote/cache-only behavior are preserved. Darwin availability uses free plus inactive kernel pages bounded to physical memory, deliberately distinct from legacy free-only Node os.freemem; Docker VM capacity remains independently bounded. Generic atomic config writes preserve existing parent-directory modes, create missing parents privately and support relative leaf paths with directory fsync. Dedicated application data directories remain private. Earlier preview artifacts remain unchanged as historical evidence.

The directory contains four platform archives, the release manifest, the exact native-only VSIX, installer and bounded verification/provenance records. `gridstator-2.0.0-dev.1-native-bundle.tar.gz` contains only the manifest and four native archives. `SHA256SUMS` covers all adjacent regular files recursively.

```sh
sha256sum --check SHA256SUMS
# macOS alternative: shasum -a 256 --check SHA256SUMS
```

For a manual disposable check, select the archive matching the host OS and architecture. Extract only its regular root `gridstator` member into a private temporary directory, then run its `build-info --json` and `release verify` commands. The verifier takes the manifest SHA256, archive, extracted executable and target (`linux/amd64`, `linux/arm64`, `darwin/amd64` or `darwin/arm64`). Exact successful commands/results are in `verification/`.

For example, after checking `SHA256SUMS`, on Linux x64:

```sh
preview_tmp=$(mktemp -d)
tar -xzf gridstator-2.0.0-dev.1-linux-amd64.tar.gz -C "$preview_tmp" gridstator LICENSE
"$preview_tmp/gridstator" build-info --json
"$preview_tmp/gridstator" release verify \
  --manifest gridstator-release.json \
  --manifest-sha256 c5e83996dac1776178bc2494021c718dfe97e96f4c5149dd8958c8a101d8d131 \
  --archive gridstator-2.0.0-dev.1-linux-amd64.tar.gz \
  --executable "$preview_tmp/gridstator" --target linux/amd64
rm -rf "$preview_tmp"
```

The installer needs ordinary OS tools and curl. It downloads a CLI into the chosen prefix; initialization, joining and service installation remain explicit later actions. No public download URL has been published. The private installer check uses a prefix containing spaces, checks repeated identical installation, initializes disposable state, serves pinned TLS status, uploads a tiny native snapshot and reads its exact bytes through a `sha256%3A` blob URL. The installed-byte smoke also checks existing absolute/relative config parents remain 0775, config files are 0600, and newly created config parents plus dedicated data directories are 0700. Target processes have no Node/npm/Rust/Python/jq in PATH; the private HTTP/TLS orchestrator runs separately. Owned actors stop and credentials/data are removed. No global prefix or live service is changed.

`PREVIEW.json` and `provenance/` distinguish each target's actual runtime and source scope from compilation. Mac Intel execution is Rosetta on M4, not physical Intel hardware. Mac binaries lack Developer ID signing/notarization, and a linker ad-hoc signature is not release signing. Historical WSL/editor/earlier runtime receipts retain their exact source/byte scope; they are not silently relabeled as this candidate's execution. Build-time metadata is preserved separately from later runtime evidence.

The five recorded stable gates remain open: `mac-signing`, `four-target-linkage`, `mac-docker-capacity`, `launchd-live` and `systemd-live`. Per-gate requirements and limitations are in `OPEN_GATES.json`. This remains an unpublished development preview. VS Code's extension host still requires Node; the controller and worker executables run natively.

Checkpoint digest ordering is the exact recorded `files` array order (pathlib path-component ordering), then UTF-8 `path + NUL + SHA256 + LF` concatenation. It is not flat full-path string sorting. The frozen source checkpoint is unchanged; `verification/checkpoint-order-verification.json` records the independent calculation.
