# Native release candidates

The [v2 upgrade plan](../docs/V2_UPGRADE_PLAN.md) defines the stable release gates. The
[candidate workflow](../.github/workflows/native-candidate.yml) builds reviewable artifacts;
it does not publish a release, update a cluster, or declare those gates passed.

Allocate one build ID before compiling any target. The workflow uses its run ID and attempt,
and passes the same `GRIDSTATOR_BUILD_ID` and full `GRIDSTATOR_SOURCE_REVISION` to four actual-host
builds. `native/scripts/set-version.mjs` stamps Cargo's package and lockfile together. Never derive
the compiled build ID from an executable/archive/manifest hash: those bytes contain the ID.

| Release key | Distribution target | Build host |
| --- | --- | --- |
| `linux/amd64` | `x86_64-unknown-linux-musl` | Linux x64 |
| `linux/arm64` | `aarch64-unknown-linux-musl` | Linux ARM64 |
| `darwin/amd64` | `x86_64-apple-darwin` | Mac Intel |
| `darwin/arm64` | `aarch64-apple-darwin` | Mac Apple Silicon |

Linux distributions are static musl binaries. Candidate checks reject ELF interpreters and
shared-library dependencies. Mac distributions use ordinary OS libraries; bundled SQLite/rustls
avoid a separate SQLite/OpenSSL installation. Build-host Rust/C tools and Node for the extension
are not target-machine dependencies.

Stable Mac jobs, and previews explicitly selecting `notarize-preview: true`, require these
repository secrets, supplied by the owner:

- `APPLE_CERTIFICATE_P12`: base64-encoded Developer ID Application signing certificate/private key.
- `APPLE_CERTIFICATE_PASSWORD` and `APPLE_SIGNING_IDENTITY` for that certificate.
- `APPLE_ID`, `APPLE_TEAM_ID`, `APPLE_APP_PASSWORD` for notarization.

In that mode each Mac binary is signed with hardened runtime and a timestamp, verified, archived in ZIP and
submitted through `notarytool`. The job requires an `Accepted` receipt. Hashes and build metadata
are collected **after** signing, against the final shipped executable. This follows Apple's
[custom notarization workflow](https://developer.apple.com/documentation/security/customizing-the-notarization-workflow).
ZIP notarization and an accepted job do not replace downloaded-binary Gatekeeper checks on both
supported Mac architectures, nor the required actual Docker/launchd/reboot validation.

Prerelease versions may select `notarize-preview: false` (the workflow default) to build unsigned
development artifacts without these credentials. They remain preview evidence and cannot pass
the signed/notarized Mac gate. Versions without a prerelease suffix always require notarization;
the preview input cannot disable it.

The assembly example accepts an input directory containing `linux-amd64`, `linux-arm64`,
`darwin-amd64` and `darwin-arm64`. Each contains `gridstator` and its actual-host
`gridstator build-info --json` output in `build-info.json`; Mac directories also contain
`notarization.json`. From `native/`:

```sh
cargo run --locked --example assemble_release -- candidate-input release --require-notarization
cd release
sha256sum --check SHA256SUMS
```

The destination must not exist. Assembly validates all four identities, architectures and final
executable hashes, then stages and atomically renames the output directory. It produces:

- Four deterministic `gridstator-<version>-<os>-<arch>.tar.gz` archives, with root `gridstator` and `LICENSE`.
- `gridstator-release.json`, with separate final executable and archive SHA-256/size fields.
- `gridstator-<version>-native-bundle.tar.gz`, containing that manifest and the four archives, for
  authenticated `controller publish-build` imports.
- `install-native.sh`, the small runtime-independent installer from the selected source revision.
- `SHA256SUMS`, covering that installer, the manifest, all archives and the native bundle.

The workflow also packages a thin native VSIX and adds its checksum. VS Code supplies its Node
runtime; the packaged native flavor requires the Rust helper. Set `gridstator.nativeCli.path`
to its verified executable in the workspace host. The retained v1 extension flavor still uses the
opt-in `gridstator.nativeCli.enabled` setting for side-by-side development. The VSIX uses the
candidate's numeric base version because VS Code's package version is separate from the native
prerelease identity.

Candidate assembly has tests for mixed identities, wrong architecture, changed signed bytes,
required Mac receipts, deterministic archives and controller bundle import. These are packaging
tests; synthetic executable headers used in those tests are not platform runtime evidence.

Manual installation uses the matching verified release archive. The small
[`install-native.sh`](../scripts/install-native.sh) selects the platform, verifies checksums and
uses the native verifier before installation. It requires ordinary OS tools and curl, without
Node/npm/Rust/Python/jq. It installs the CLI; `controller init`, `agent join` and `install-service`
remain explicit operations. Use isolated v2 configuration, data and services throughout rehearsal.

Before stable publication, record every required parity/platform result in the
[ledger](contracts/parity-ledger.json), including actual Mac and ARM hosts, WSL2, service startup,
late Docker, update failure/rollback, signed downloaded artifacts and the fresh-cluster recovery
runbook. The legacy npm release workflow only handles v0/v1 tags. Native v2 publication stays
separate until the agreed release review has accepted the evidence.

The concrete clean installation, manual-download, stopped-v1 archive/fresh-v2 cutover and separate
v1 recovery procedure is in [CUTOVER.md](CUTOVER.md). Its opt-in `recovery_e2e` builds actual static
Linux candidates and exercises authenticated controller update, health confirmation, crash rollback
and restoration after a schema-changing failed executable. This is Linux-host evidence; other-target
bundle entries in that rehearsal are explicitly parsing fixtures and do not pass their runtime gates.
For concrete candidate acquisition and M4/Docker Desktop/launchd, ARM Ubuntu VM and Windows WSL2
validation steps, use [VALIDATION_HOSTS.md](VALIDATION_HOSTS.md).

## Build the Linux executables through Gridstator

The repository's [gridstator.yml](../gridstator.yml) defines `native-linux`, `native-linux-x64`
and `native-linux-arm64` tasks. They run [build-linux.sh](scripts/build-linux.sh) inside a
digest-pinned Rust 1.99.1 Bookworm image, with 2 CPUs/4 GiB and persistent Cargo caches under
`/gridstator/cache`. Checksum-pinned Zig and Rust LLD provide musl C compilation and linking.
The build host is a Docker worker; the target installation still needs no Node or Rust runtime.

With the native CLI connected to a disposable Docker-ready native cluster, submit from the
repository root:

```sh
gridstator run --task native-linux \
  --env "GRIDSTATOR_SOURCE_REVISION=$(git rev-parse HEAD)" --detach --json
job_id=replace-with-submitted-id
gridstator job "$job_id" --json
gridstator logs "$job_id"
gridstator artifacts "$job_id" --download native/self-build.tar --out /absolute/new/path/self-build.tar
```

Replace the job ID with the submitted ID and use a new artifact output path. The returned tar
contains both `linux-amd64` and `linux-arm64` executables, separate hashes/build metadata/linkage
reports and `SOURCE_SHA256SUMS`. A single randomly allocated build ID identifies both targets;
set `GRIDSTATOR_BUILD_ID` explicitly through `--env` when allocating an agreed build identity.
The Git revision is base provenance for a dirty working tree; retain the actual submitted snapshot
ID and manifest. Static linkage and successful cross-compilation do not establish ARM runtime
support. Darwin builds remain the separate actual-Mac CI jobs.

The [recorded Linux self-build](contracts/evidence/self-build-linux-2026-11-10/README.md) exercised
that real task, downloaded its artifact, matched all reported source hashes against the exact
snapshot and ran the x64 executable in Alpine without Node/Rust. It is working-tree preview
evidence, separate from final signed release qualification.
