# Gridstator builds its Linux native executables

An actual native CLI/controller/embedded worker ran the repository's Docker task successfully on
Ubuntu x86_64 on 2026-11-10. The controller used private temporary state, an immutable copied CLI
and an ephemeral loopback port. No existing service or cluster was used.

From the repository root, the submitted command was:

```sh
gridstator run --task native-linux \
  --env GRIDSTATOR_SOURCE_REVISION=4b8f13ccf1b21820007fef0ab52143c961b69216 \
  --detach --json
```

- Job: `01M71GH6KGBEDGCTWJSP0HDMFE`, succeeded with exit 0.
- Snapshot: `sha256:de20de76d20d4ba69358f3bf03e61b30ec8a21bdb7f6b6a69ac6a25ed7565bd7`.
- Shared build ID: `native-selfbuild-c7ad6c10-bdc4-4027-9d84-942ce87578d6`.
- Version: `2.0.0-dev.1`.
- Running time: 596.672 seconds; submission through completion: 630.524 seconds.

This is an **uncommitted working-tree snapshot**. The Git SHA identifies its base commit, not all
its content. `snapshot-manifest.json` contains the exact 404 submitted entries (4,743,681 source
bytes), and its own SHA-256 equals the recorded snapshot ID. `snapshot-source-files.json` identifies
the task, build script, Cargo files and pinned toolchain within it. Later shared workspace edits
are outside this proof.

The real command executor pulled
`rust:1.99.1-bookworm@sha256:e6f2cff35d0f309acf8433b4f05878ed77092602c5065cc81801177da0bb3a9c`
and ran `bash native/scripts/build-linux.sh both`. Actual Docker inspection confirmed 2 CPUs,
4 GiB RAM, a 4 GiB combined memory/swap limit, and the persistent `gridstator-cache` volume mounted
at `/gridstator/cache`. The script downloaded checksum-pinned Zig 0.17.1, used Rust 1.99.1 and Rust
LLD, and compiled both musl targets. No source, script or task correction was needed during the
proof. The snapshot emitted an unused-import warning in `src/client.rs`; compilation succeeded.

| Artifact | Bytes | SHA-256 | Execution evidence |
| --- | ---: | --- | --- |
| `linux-amd64/gridstator` | 27,182,456 | `aa916aa57ef7a1653588dd224e0e3f1e563450e7ed593d7ed96e1f966e973883` | Build-info, version and private controller initialization in Alpine |
| `linux-arm64/gridstator` | 26,196,872 | `3116b21b94d97ddd5053a3bf1486e0e4a50db370cfce494c595a014eb4167d57` | Cross-compiled; not executed |

The returned job artifact `native/self-build.tar` was downloaded through the native artifact API
and extracted privately. Its SHA-256 is
`384590746fca3a4ccdf64c25852c12e5bcc0999d7d5743af733f17d7c797a270`.
Independent `file` and `readelf -h -l -d` inspections identified the correct ELF machine for each
binary, with no `INTERP`/`NEEDED` entries. Sizes/hashes match their build metadata. All 106 source
entries in the returned `SOURCE_SHA256SUMS` match the submitted canonical snapshot manifest.

The x64 executable ran in a read-only, network-disabled Alpine 3.21 container with only `/tmp`
writable. The probe required Node/npm/Rust/Cargo/Zig to be absent from PATH, then ran the actual
binary's `build-info --json`, `--version` and private `controller init`. The actual reported binary
hash/build ID match the artifact metadata. `runtime-image.json` identifies that precise Alpine
image; `linux-amd64-alpine-runtime.log` is its output with the newly generated admin token redacted.
This smoke test does not replace the broader controller/worker/executor acceptance suite.

The controller exited 0 after the completed job. Its owned job container, workspace volume and
controller process were absent after cleanup. The shared persistent build cache was retained.
No service definitions were installed or changed. Original private state remains at
`/tmp/gridstator-self-build-27roilfr`; generated binaries, source manifest and artifact tar also
remain under ignored `native/self-build/proof-01M71GH6KGBEDGCTWJSP0HDMFE/`.

These files contain job/snapshot observations, source manifests, build and runtime metadata,
linkage inspection and sanitized logs. Client/agent credentials, CA private keys and databases
are excluded. Credential-like token strings were scanned before copying. `SHA256SUMS` verifies
the saved evidence files. Neither ARM runtime, Darwin, WSL2 nor final release provenance is claimed.
