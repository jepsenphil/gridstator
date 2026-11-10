# Gridstator native development preview

Version `2.0.0-dev.1`, build `native-preview-20261110-95cfa7161440`. These are four real native executables assembled from the checkpointed, uncommitted working tree. The embedded revision is the parent reference commit, not a claim that these bytes came from that clean commit. See `SOURCE_CHECKPOINT.json`, `SUPPLEMENTAL_BUILD_INPUTS.json` and `PREVIEW.json`.

This directory has not been published. It contains four platform archives, a release manifest, the native-only VSIX, the installer, and verification records. `gridstator-2.0.0-dev.1-native-bundle.tar.gz` contains only the manifest and four native archives. The adjacent files, including the VSIX and evidence, are covered by `SHA256SUMS`.

Verify the complete directory before selecting an archive:

```sh
sha256sum --check SHA256SUMS
# macOS alternative: shasum -a 256 --check SHA256SUMS
```

For a manual, disposable check, choose the archive matching the host OS and architecture. Extract only its regular root `gridstator` member into a private temporary directory, then run its `build-info --json` and `release verify` commands. The verifier takes the manifest SHA256, archive, extracted executable, and target (`linux/amd64`, `linux/arm64`, `darwin/amd64` or `darwin/arm64`). The exact successful Linux invocation and result are retained in `verification/installer-runtime-smoke.json` and `verification/manual-verification.json`.

The installer requires ordinary OS tools and curl. It downloads a CLI into the chosen prefix; cluster initialization, joining and service installation are explicit later actions. There is no public download URL for this unpublished directory. `verification/installer.log` records a successful private loopback download and repeated identical install into a private prefix containing spaces. The installed CLI initialized disposable state, served authenticated pinned TLS status, and stopped cleanly with Node, npm, Rust, Python and jq absent from its PATH. The private HTTP server and controller stopped, and temporary credentials/data were removed. No live service or global prefix changed.

The final Linux x86-64 bytes also ran in a clean, network-disabled, read-only Alpine container. Linux ARM64 was cross-built and checked for correct ELF headers, static linkage, size and digest; it was not executed. Both final Mac binaries ran foreground controller/worker and private LaunchAgent lifecycle checks on an M4. The Intel binary ran under Rosetta, not physical Intel hardware. Mac ARM64 has only its linker ad-hoc signature; Mac x86-64 is unsigned. Neither has Developer ID signing or notarization.

This is a development preview. Physical Linux ARM64 and Intel Mac runtime acceptance, signing/notarization, clean release provenance and stable release approval/publication remain open. Earlier broad Mac/editor/update proofs retain their recorded checkpoint scope. `provenance/` contains bounded build/runtime summaries; the full repository evidence paths are named in those records. The native-only VSIX still runs within VS Code's Node extension host and uses these native controller/worker tools.
