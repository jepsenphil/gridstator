# Native Gridstator development

Date: 2026-11-10. This package implements the confirmed [v2 plan](../docs/V2_UPGRADE_PLAN.md)
alongside the retained TypeScript reference. The development preview has 223 of 228 acceptance
rows verified; five platform/service/signing gates remain open. See [progress](../docs/V2_PROGRESS.md)
for the current artifacts and the exact scope of the evidence.
Do not point native commands at v1 data, credentials or services. Use a separate temporary directory
and the `gridstator-v2` development data/config defaults and `gridstator-native-*` service names.

From this directory, use the pinned toolchain in `rust-toolchain.toml`:

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets -- --test-threads=1
cargo build --locked --release
./target/release/gridstator --help
./target/release/gridstator --version
```

The v2 editor artifact is packaged separately from the retained v1 extension:

```sh
pnpm --filter ./apps/vscode package:native
```

Run that command from the repository root. It produces `apps/vscode/dist/gridstator-native.vsix`
on VS Code's prerelease channel, with native delegation required and no bundled JavaScript CLI or
snapshot implementation. Install the Rust CLI on the workspace host, including the remote host for
SSH/WSL. The extension checks an explicit `gridstator.nativeCli.path`, the installer's default
`~/.local/bin/gridstator`, then safe absolute PATH entries. It uses the separate native login and
requires workspace trust. The ordinary extension build remains available as the v1 reference.

The native build and fixture tests require Rust and build/linker tools. They do not invoke Node or
pnpm. The target-machine requirement is a standalone executable plus normal OS facilities and
external worker tools: Docker, buildx for builds, and act/git for Actions. Bundled SQLite and rustls
must avoid separately installed SQLite/OpenSSL runtime dependencies. A build on a developer machine
alone does not prove a clean-machine installation. Dependency/linkage, minimum OS versions and
the complete platform/runtime matrix remain acceptance work; see [progress](../docs/V2_PROGRESS.md).

Worker CPU and memory reservations are amounts withheld from the execution pool. That pool is the
smaller of reported host capacity and Docker execution capacity, then reduced by reservations;
live host pressure adds a further placement constraint. Values are CPUs and MiB in the API, with
GiB shown in the editor. Actual M4 Docker VM bounds and readiness recovery have separate evidence;
physical Intel capacity and the full service/late-start lifecycle still require acceptance.

The accepted v1 reference is commit `4b8f13ccf1b21820007fef0ab52143c961b69216`, whose package version
is `0.1.0`. [Baseline metadata](contracts/accepted-v1/baseline.json) records 190 source hashes, the
intentional confirmed-plan documentation overlay and fixture hashes. The reference directory is
immutable: corrections or later accepted v1 changes require a separate baseline directory and an
explicit ledger review. Do not regenerate expected data from Rust.

[Contract fixtures](contracts/accepted-v1) retain canonical manifest bytes/UTF-8 hex/IDs, malicious
manifest rejections, the full job transition matrix, schema defaults/bounds/null/unknown-field
behavior, error/units, and every registered CLI help command plus parser process examples. Captured
help normalizes the hostname and uses explicit data/config paths. It is Linux help evidence; it does
not substitute for platform-specific path/service acceptance. Zod issue wording is retained as
reference diagnostics; native schema acceptance compares parsed data and acceptance/rejection.

To reproduce the frozen reference, run from the repository root after `pnpm install --frozen-lockfile`:

```sh
node native/scripts/capture-v1.mjs --check
node native/scripts/check-ledger.mjs
```

`node native/scripts/check-ledger.mjs --release` additionally fails while required acceptance rows
remain open. An approved exception still requires passed replacement evidence before this gate
closes. Passing structural ledger validation alone does not establish release readiness.

Reference checking requires Git history, Node/pnpm, unchanged frozen v1 source and a TypeScript build;
it always rebuilds reference code before capture. If retained TypeScript files have since changed,
run it in a separate checkout of the recorded revision, copying `native/scripts` and
`native/contracts` from this tree into that checkout. CI does this in `.v1-reference`; it never
rewrites the active TypeScript working tree. It never starts a service or submits a job. The
script checks all indexed bytes against the recorded commit and refuses extra reference inputs.
`--output <new-directory>` can capture a separately reviewed baseline, and refuses an existing
directory. The [parity ledger](contracts/parity-ledger.json) tracks individual behavior cases, owners,
acceptance and evidence. TypeScript test names and captured reference fixtures do not count as native
verification. Plan-approved differences live in [compatibility exceptions](contracts/compatibility-exceptions.json);
[known v1 limitations](contracts/known-v1-limitations.json) remain distinct from native defects.

[Native CI](../.github/workflows/native.yml) proposes four actual-host build/unit/CLI-smoke jobs:

| Release key | Rust target | Actual hosted runner |
| --- | --- | --- |
| `linux/amd64` | `x86_64-unknown-linux-gnu` | `ubuntu-24.04` |
| `linux/arm64` | `aarch64-unknown-linux-gnu` | `ubuntu-24.04-arm` |
| `darwin/amd64` | `x86_64-apple-darwin` | `macos-15-intel` |
| `darwin/arm64` | `aarch64-apple-darwin` | `macos-15` |

Runner labels follow [GitHub's runner reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners).
A job checks the actual Rust host and OS/architecture before recording its result. It saves exact
binary SHA-256, compiler/OS identity, CLI help/version and linkage. If a required runner is unavailable,
that target remains unverified. The workflow has not been run by adding its source file.

You can record the same bounded linkage/smoke evidence on an actual target host:

```sh
bash scripts/audit-native.sh target/release/gridstator linux/amd64 /tmp/gridstator-native-evidence
```

Choose the matching release key from the table. With `CARGO_BUILD_TARGET` set, the binary instead
lives at `target/<rust-target>/release/gridstator`. Cross-compilation success is build evidence only;
it is never accepted by this script as target-host runtime evidence. Linkage checks reject external
Node, SQLite and OpenSSL libraries, but do not establish clean-machine installation by themselves.

CI artifacts are development outputs. There is no native publishing job. Actual Docker/buildx/act,
service lifecycle, Mac Docker VM capacity/late startup, reboot, WSL2, VS Code, update/rollback,
signing/notarization and clean-target installation require separate evidence. Current completed
checks and the five remaining gates are recorded in [progress](../docs/V2_PROGRESS.md). Hosted Mac ARM runners
[cannot run nested virtualization](https://docs.github.com/en/actions/reference/runners/github-hosted-runners#limitations-for-arm64-macos-runners),
so their build/CLI success cannot close Docker Desktop acceptance. Never silently skip a required
real-tool scenario and report it as passed.
