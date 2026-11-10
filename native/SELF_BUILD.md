# Building Gridstator with Gridstator

The repository's `gridstator.yml` provides Linux build tasks. A worker runs the pinned Rust image
against the captured working tree; its OS account does not need Rust, Zig, Node or a C compiler.
The build container supplies Rust and downloads a checksum-pinned Zig C cross compiler. Docker
remains the worker prerequisite.

From the repository root, using a disposable native cluster or an explicitly selected development
cluster:

```sh
gridstator run --task native-linux \
  --env "GRIDSTATOR_SOURCE_REVISION=$(git rev-parse HEAD)"
```

`native-linux` builds both `linux/amd64` and `linux/arm64` sequentially with a common allocated build
ID. `native-linux-x64` and `native-linux-arm64` select one target. All tasks request two CPUs and
4 GiB, with a one-hour timeout. On a Mac worker, configure enough Docker VM capacity after the worker
reservation; the Rust container runs Linux even though the worker itself is a native Mac executable.
The cached Cargo registry and target files live under the worker's `/gridstator/cache` mount.

The `native/self-build.tar` job artifact contains the executables, build metadata, checksums, static
linkage reports and a source-file hash manifest. List/download it through the usual interface:

```sh
gridstator artifacts JOB_ID
gridstator artifacts JOB_ID --download native/self-build.tar --out /tmp/gridstator-linux-build.tar
```

The recorded Git revision can identify the working tree's base commit while uncommitted edits are
present. The Gridstator snapshot ID and source hash manifest identify the actual input bytes.
Metadata deliberately labels this as a working-tree build and leaves runtime validation open.
Cross-building an ARM64 executable does not establish ARM64 runtime or service acceptance.

To coordinate identities across separately built targets, provide the same explicit
`GRIDSTATOR_BUILD_ID` and full `GRIDSTATOR_SOURCE_REVISION` to each build. Each new build invocation
must receive a new build ID. Final release assembly additionally requires matching versions, all four
target artifacts, verified final bytes and the remaining release evidence.

Mac build/signing remains in [Mac CI](../.github/workflows/native-candidate.yml), as agreed: native
host execution is outside Gridstator's Docker execution model. The candidate workflow can assemble
unsigned prerelease previews without Apple credentials. Stable versions always require Developer ID
signing and notarization. It currently builds its own four candidate artifacts; the self-build tasks
produce separately reviewable development outputs and do not automatically publish or promote them.
See [validation hosts](VALIDATION_HOSTS.md) and [release assembly](RELEASING.md).
