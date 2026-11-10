Final Linux self-build: `native-preview-20261110-cd6b769ebe6e`

Gridstator job `01M71Y4MXHK68CVPM8DM1VTXYA` built both static Linux targets from canonical snapshot `sha256:f2fe505a2e561009cc08c7d7126ec3fde200aa248f6a9ae667d5bee3e0d66946`. All 40 production/build checkpoint inputs, three supplemental inputs and 763 artifact source-manifest entries match that exact snapshot. The source revision is the parent reference commit of an uncommitted preview.

The actual x64 output ran version/build-info, isolated controller initialization, pinned TLS status and graceful stop in a read-only, network-disabled Alpine image without Node/npm/Rust/Cargo/Zig. ARM is build-only evidence here; separate actual ARM runtime validation must be read on its own. Both binary hashes and ELF/static-linkage output are retained.

The immutable ignored delivery directory is `native/self-build/proof-01M71Y4MXHK68CVPM8DM1VTXYA`. `evidence.json` records all identities, limits and timing. Predecessor previews remain unchanged. Only `native/src/worker.rs` differs from the prior 1fd checkpoint.

The disposable foreground controller and embedded worker stopped; their containers/workspaces are absent, credentials/state were removed, and shared Cargo build caches were retained. No existing service, live cluster, release signing or publication was involved.
