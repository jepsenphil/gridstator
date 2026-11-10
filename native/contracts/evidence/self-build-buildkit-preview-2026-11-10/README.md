# Historical BuildKit preview self-build

`native-preview-20261110-1fd6e3d425b3` successfully built both Linux targets through the
actual private Gridstator CLI/controller/embedded worker and Docker. This preview is
frozen historical evidence: a later M4 workflow found a Darwin host-memory sampling
defect, whose correction is outside this snapshot. It is not the final delivery candidate.

- Job: `01M71WTMJG104TA719VT67QRY3`, succeeded with exit 0.
- Snapshot: `sha256:e6a4bc94454017ec988ccdeffcdd901fe049be8b820f5f8ba4f7b7bfcd1d8856`.
- Version: `2.0.0-dev.1`; source revision is the parent commit of an uncommitted preview.
- Run time: 440.779 seconds; submission through completion: 479.429 seconds.
- All 40 checkpoint inputs, three supplemental inputs and 649 returned source-manifest
  entries match the canonical 967-entry snapshot (8,090,189 source bytes).

| Target | Bytes | SHA-256 | Actual execution |
| --- | ---: | --- | --- |
| Linux x64 | 27,608,144 | `85564631a2c3ca785401da53ab13cc27fcf47da84796f91b7aa785392e9e9577` | Clean Alpine version/build-info/init, live TLS status and graceful stop |
| Linux ARM64 | 26,568,520 | `18e2c8464d912ede9021b88d203fae4b69da584321517875500569b008748314` | Not executed by this proof |

The returned 54,274,560-byte artifact tar matches the API digest
`7e254b6c488e9f3d9ceb8321b0e8db29db3c7eaaedea2385d879e674be607033`.
Independent inspection confirms both ELF machines and no interpreter/dynamic dependencies.
Actual container metadata confirms two CPUs, 4 GiB RAM, the persistent build cache and
Rust/Cargo 1.99.1 with Zig 0.17.1. The x64 smoke ran read-only and network-disabled without
Node/npm/Rust/Cargo/Zig on PATH. Original build-time `runtimeValidated:false` metadata is
preserved; later execution observations are separate.

Only `native/src/executor/build.rs` differs from the preceding `e3e5fae51c8e` production
checkpoint. Its load/tar image-ID fallback handles BuildKit omitting the config digest.
Both older preview directories remain unchanged.

Payloads and the tar remain under ignored
`native/self-build/proof-01M71WTMJG104TA719VT67QRY3/`. `evidence.json` records commands,
source/checkpoint identities, paths and limits. The owned controller exited 0; independent
checks found no owned process/container/workspace volume. Shared caches remained; private
credentials, keys and databases were removed. `SHA256SUMS` covers credential-scanned evidence.

No Darwin memory-fix, ARM runtime, WSL2, service/reboot, signing or publication acceptance
is claimed here.
