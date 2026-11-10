# Replacement native preview builds itself

The frozen production tree built both Linux native executables through an actual private
Gridstator CLI/controller/embedded worker and Docker on Ubuntu x86_64. The job succeeded
with exit 0 in 444.135 seconds; submission through completion took 458.146 seconds. No
production or build correction was made during the proof.

- Preview build ID: `native-preview-20261110-e3e5fae51c8e`.
- Version: `2.0.0-dev.1`.
- Job: `01M71VMMVXGSK5XNEATVAHHFDP`.
- Canonical snapshot: `sha256:216fa791057a14e9cad1e2c2e8b4b07d22fc3c1a6a2de9d617226e30a82de153`.
- Production checkpoint: `e3e5fae51c8e12f26935ab2d8aabaef85175c5fe81d9fe0fd843ffc6ea8b4b5c`.


This replaces the historical `native-preview-20261110-95cfa7161440` preview after the
controller GET-blob URI compatibility correction. Comparing the two production
checkpoints identifies only `native/src/controller.rs` as changed: the digest is decoded
once with the existing safe path decoder before validation, so raw and escaped-colon SDK
requests work. The previous preview and its evidence remain byte-for-byte frozen.

The build ID was allocated before compilation and is shared with the separately compiled
Mac preview outputs. This is an **uncommitted working-tree preview**. Source revision
`4b8f13ccf1b21820007fef0ab52143c961b69216` identifies its parent reference commit; it is not
a clean-release provenance claim. The exact uploaded snapshot contains 827 entries and
7,216,861 source bytes. All 40 production/build inputs match the recorded checkpoint and
canonical snapshot. The license, embedded installer and YAML recipe match as separate
supplemental inputs. All 510 entries in the returned `SOURCE_SHA256SUMS` also match that
snapshot. The checkpoint document's own hash is recorded separately from its production
checkpoint hash.

From the repository root, the immutable copied native CLI submitted:

```sh
gridstator run --task native-linux \
  --env GRIDSTATOR_SOURCE_REVISION=4b8f13ccf1b21820007fef0ab52143c961b69216 \
  --env GRIDSTATOR_BUILD_ID=native-preview-20261110-e3e5fae51c8e \
  --detach --json
```

Actual Docker inspection confirmed the configured immutable Rust image, two CPUs, 4 GiB
RAM, equal memory/swap limit, and persistent `gridstator-cache` at `/gridstator/cache`.
Probes inside the owned build container reported Rust/Cargo 1.99.1 and Zig 0.17.1. The
source recipe verifies the Zig archive against its pinned checksum and uses Rust LLD for
both musl targets. Build container/image and tool identities are recorded in their JSON
files.

| Artifact | Bytes | SHA-256 | Execution evidence |
| --- | ---: | --- | --- |
| Linux x64 | 27,598,768 | `d408a50bf80a4e430b55ccf3fcdec38e48534ac79e52db35025d38050de6e601` | Clean Alpine version/build-info/init, live TLS controller status and graceful stop |
| Linux ARM64 | 26,578,344 | `995d69b467dc571117e84a580b29d9df406e5d6d6ed110215997e29eedb4ebf7` | Cross-compiled and inspected; not executed |

The actual returned `native/self-build.tar` is 54,255,104 bytes, SHA-256
`12f6fcda27d6cfd5b012655d9b93d606648969cb5be77dbe5bd5ed36fc73b251`.
Its size and digest match the native artifact API. Independent file/readelf inspection
confirms each ELF machine, with no interpreter or dynamic library dependencies. Binary
sizes, hashes, version, build ID and source revision match their generated metadata.

The x64 executable ran in read-only, network-disabled Alpine with only `/tmp` writable.
The probe required Node/npm/Rust/Cargo/Zig to be absent, then ran build-info, version,
private cluster initialization, an actual TLS controller, authenticated status, and a
graceful shutdown. Its actual build ID and binary digest match the artifact metadata.
The successful sanitized runtime output is retained. This smoke does not repeat
the full executor/service suite against this static artifact.

Ignored delivery copies are available under
`native/self-build/proof-01M71VMMVXGSK5XNEATVAHHFDP/`, including both binaries and original
build metadata. Their build-time `runtimeValidated:false` is preserved; the later x64
execution result is recorded separately in `artifact-validation.json`. The package
assembler consumes the same bytes for the real four-target unsigned development bundle.

The owned controller exited 0. Independent post-proof checks found no owned role process,
container or workspace volume; the shared build cache remained intact. No service was
installed or changed. Private client/agent credentials, keys and databases were removed.
Only credential-scanned metadata, manifests and logs are preserved here; `SHA256SUMS`
covers them.

No ARM runtime, physical Intel Mac, WSL2, OS-service/reboot, signing/notarization, stable
release or public deployment is claimed by this proof.
