# Native Gridstator development

Gridstator 2.0.0 is planned as the first public native release. The Rust executable contains the CLI, controller and worker. Five [release acceptance gates](contracts/release-acceptance.json) remain open; see [progress](../docs/NATIVE_PROGRESS.md) and the [release plan](../docs/NATIVE_RELEASE_PLAN.md).

Use the pinned toolchain in `rust-toolchain.toml`:

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets -- --test-threads=1
cargo build --locked --release
./target/release/gridstator build-info --json
```

From the repository root, `pnpm --filter ./apps/vscode package:native` builds the extension. Every VSIX requires the native executable on its workspace host; for Remote SSH or WSL that is the remote host. The extension resolves `gridstator.nativeCli.path`, the installer's `~/.local/bin/gridstator`, then safe absolute PATH entries. VS Code supplies the extension's JavaScript runtime.

[Behavior fixtures](contracts/behavior/) and the other JSON documents in `contracts/` exercise native manifests, state transitions, schema boundaries, CLI help, scheduling, URL handling and workspace patterns. They are test inputs, not a separate runtime. The `/v1` HTTP route and capability labels version the internal wire contract.

Check the release record locally:

```sh
node scripts/check-release-acceptance.mjs
candidate_sha=replace-with-validated-candidate-commit
node scripts/check-release-acceptance.mjs --release --revision "$(git rev-parse HEAD)" --candidate-revision "$candidate_sha"
```

The first command validates the record. The release form intentionally fails until both Linux stable gates contain verified final-candidate evidence. Mac signing, Docker and launchd remain separate future stable Mac gates. Passing unit tests alone does not close actual-host runtime or service requirements.

[Native CI](../.github/workflows/native.yml) tests Rust on actual hosted Linux x64/ARM64 and Mac Intel/Apple Silicon runners. The [candidate workflow](../.github/workflows/native-candidate.yml) builds reviewable artifacts without publication. [Validation hosts](VALIDATION_HOSTS.md), [testing](TESTING.md) and [release procedure](RELEASING.md) describe the runtime and artifact checks. Cross-compilation, a hosted build or a fixture screenshot cannot substitute for actual target-host evidence.
