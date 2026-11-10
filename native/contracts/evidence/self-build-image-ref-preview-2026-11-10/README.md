Final Linux self-build: `native-preview-20261110-7c41b2f3c79d`

Gridstator job `01M72202QV3TGHQEHTC67VN3HJ` built both static Linux targets from canonical snapshot `sha256:862ceb25d170dc482040f1d90a552ef6e9eb764ee20530b501070f3453b89298`. All 40 production/build checkpoint inputs, three supplemental inputs and 1037 artifact source-manifest entries match that exact snapshot. The source revision is the parent reference commit of an uncommitted preview.

The actual x64 output ran version/build-info, isolated controller initialization, pinned TLS status and graceful stop in a read-only, network-disabled Alpine image without Node/npm/Rust/Cargo/Zig. The exact ARM output passed a separate whole native workflow on the actual M4 Docker ARM Linux VM; that receipt is included separately and does not imply systemd/boot support. Both binary hashes and ELF/static-linkage output are retained.

The immutable ignored delivery directory is `native/self-build/proof-01M72202QV3TGHQEHTC67VN3HJ`. `evidence.json` records all identities, limits and timing. Predecessor previews remain unchanged. Only `native/src/executor/build.rs` differs from the prior cd6 checkpoint.

The disposable foreground controller and embedded worker stopped; their containers/workspaces are absent, credentials/state were removed, and shared Cargo build caches were retained. No existing service, live cluster, release signing or publication was involved.

The independent exact final ARM receipt is `linux-arm64-workflow-proof.json`, with mounted binary/tool hashes and post-cleanup resource audit in `linux-arm64-cleanup-audit.json`. It passed all6 default strict scenarios, exit0, in6.647seconds: native CLI/controller and embedded/standalone ARM worker command/snapshot/artifact, image reference+size+execution, verified external act shell step, overlapping shards/combinedJUnit, cancellation cleanup and doctor. The worker PATH has no Node/npm/Rust/Cargo/Python; Python orchestrates on macOS outside the container. This actual ARM Linux VM proof is separate from systemd/boot and physical IntelMac acceptance.
