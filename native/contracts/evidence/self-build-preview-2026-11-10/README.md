# Final native preview builds itself

The frozen production tree built both Linux native executables through an actual private
Gridstator CLI/controller/embedded worker and Docker on Ubuntu x86_64. The job succeeded
with exit 0 in 399.239 seconds; submission through completion took 412.426 seconds. No
production or build correction was made during the proof.

- Preview build ID: `native-preview-20261110-95cfa7161440`.
- Version: `2.0.0-dev.1`.
- Job: `01M71RTC1RCA4FHFE8BZBF8G94`.
- Canonical snapshot: `sha256:dd2b1ed35bad1b4d85a57b70f65b7c50d15ba068664e32670db93b7189bee54b`.
- Production checkpoint: `95cfa71614400b4d726f9c6caccbd87d75e4e003701bd753879f8158e580e3e4`.

The build ID was allocated before compilation and is shared with the separately compiled
Mac preview outputs. This is an **uncommitted working-tree preview**. Source revision
`4b8f13ccf1b21820007fef0ab52143c961b69216` identifies its parent reference commit; it is not
a clean-release provenance claim. The exact uploaded snapshot contains 739 entries and
6,764,881 source bytes. All 40 production/build inputs match the recorded checkpoint and
canonical snapshot. The license, embedded installer and YAML recipe match as separate
supplemental inputs. All 422 entries in the returned `SOURCE_SHA256SUMS` also match that
snapshot. The checkpoint document's own hash is recorded separately from its production
checkpoint hash.

From the repository root, the immutable copied native CLI submitted:

```sh
gridstator run --task native-linux \
  --env GRIDSTATOR_SOURCE_REVISION=4b8f13ccf1b21820007fef0ab52143c961b69216 \
  --env GRIDSTATOR_BUILD_ID=native-preview-20261110-95cfa7161440 \
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
| Linux x64 | 27,598,104 | `8f9adecdeb1eff5a07824239c9d692298b10728b766184cb36143c8cfa471eed` | Clean Alpine version/build-info/init, live TLS controller status and graceful stop |
| Linux ARM64 | 26,578,192 | `036440b41b6e7e9374c00634f7bc9a1fcdcbb575490ad057facc354859b387df` | Cross-compiled and inspected; not executed |

The actual returned `native/self-build.tar` is 54,242,816 bytes, SHA-256
`d436551e0aed887331a261377c2fb323d104e1253254ed9376c68980bfa746d0`.
Its size and digest match the native artifact API. Independent file/readelf inspection
confirms each ELF machine, with no interpreter or dynamic library dependencies. Binary
sizes, hashes, version, build ID and source revision match their generated metadata.

The x64 executable ran in read-only, network-disabled Alpine with only `/tmp` writable.
The probe required Node/npm/Rust/Cargo/Zig to be absent, then ran build-info, version,
private cluster initialization, an actual TLS controller, authenticated status, and a
graceful shutdown. Its actual build ID and binary digest match the artifact metadata.
The smoke harness needed a shell backreference quoting correction; the binary and build
were unchanged. The successful sanitized output is retained. This smoke does not repeat
the full executor/service suite against this static artifact.

Ignored delivery copies are available under
`native/self-build/proof-01M71RTC1RCA4FHFE8BZBF8G94/`, including both binaries and original
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
