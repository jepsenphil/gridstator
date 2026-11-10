Final Linux self-build: `native-preview-20261110-6994d2cc804b`

Gridstator job `01M724AVW456E5EY8W9BJ824T7` built both static Linux targets from canonical snapshot `sha256:de1db0f2e0ae975ea0f4f080ac5716b75370192eadfe951b9d4a6dc8eaf47cde`. All 40 production/build checkpoint inputs, three supplemental inputs and 1174 artifact source-manifest entries match that exact snapshot. The source revision is the parent reference commit of an uncommitted preview.

The actual x64 output ran version/build-info, isolated controller initialization, pinned TLS status and graceful stop in a read-only, network-disabled Alpine image without Node/npm/Rust/Cargo/Zig. The exact ARM output passed a separate whole native workflow plus configpermissioncases on the actual M4 Docker ARM Linux VM; the receipt is included separately and does not imply systemd/boot support. Both binary hashes and ELF/static-linkage output are retained.

The immutable ignored delivery directory is `native/self-build/proof-01M724AVW456E5EY8W9BJ824T7`. `evidence.json` records all identities, limits and timing. Predecessor previews remain unchanged. Only `native/src/state.rs` differs from the prior7c checkpoint.

The disposable foreground controller and embedded worker stopped; their containers/workspaces are absent, credentials/state were removed, and shared Cargo build caches were retained. No existing service, live cluster, release signing or publication was involved.

Final-byte permission/runtime receipts

The exactx64 Alpine runtime additionally passed absolute/relative existingparent0775 preservation, configfile0600, newnestedparents0700 and dedicateddata0700. Its public receipt and exactcheckscript are `linux-amd64-config-parent-permissions.json` and `linux-amd64-runtime-check.sh`. All generated private state was in the disposable container tmpfs and was destroyed.

The exactARM receipt `linux-arm64-workflow-proof.json` passed6 defaultstrict core scenarios plus3 equivalentpermissioncases, exit0, in7.35seconds. `linux-arm64-config-parent-permissions.json` records exactCLIcommands and modes without rawconfig/output. `linux-arm64-cleanup-audit.json` independently confirms exact mountedcore/tool hashes, absent ownedcontainer/statevolume/tag and emptyper-node resource labels; generated modeprobe credentials were also destroyed. This is ARM LinuxVM runtime evidence, with systemd/boot and physicalIntelMac gates remainingseparate.
