# Contributing to Gridstator

Gridstator is an Apache-2.0 open source project. The Rust CLI/controller/worker and native VS Code extension are the current development line. Issues, reproducible bug reports and focused pull requests are welcome.

## Start

Read the [architecture](docs/ARCHITECTURE.md) and [development guide](docs/DEVELOPMENT.md). Use the pinned Rust toolchain and pnpm version, then run the checks relevant to your change:

~~~sh
cd native
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets -- --test-threads=1

# From the repository root, for extension/TypeScript changes:
cd ..
pnpm install --frozen-lockfile
pnpm build
pnpm typecheck
pnpm lint
pnpm test
pnpm --filter ./apps/vscode package:native
~~~

The real Docker, BuildKit, `act`, VS Code and platform suites are listed in [native/TESTING.md](native/TESTING.md). A test skipped for missing tools does not count as release evidence.

## Pull request expectations

- Update the relevant user guide and architecture/security material in the same PR as behavior changes.
- Add a regression scenario for security-sensitive code: auth, TLS, snapshots, filesystem paths, container execution, secrets, updates and rollback.
- Keep useful behavior fixtures and native tests in sync with changes; record platform evidence for release gates.
- Do not add a failing security scanner to `continue-on-error` or silently suppress an advisory. An exception needs an owner, exposure analysis and expiry.
- Do not include tokens, host credentials or private cluster data in screenshots, logs or issues.

[CI](.github/workflows/ci.yml), [native checks](.github/workflows/native.yml) and [security checks](.github/workflows/security.yml) run on pull requests. Maintainers can also launch the root [Gridstator tasks](gridstator.yml) from VS Code's Run view.

## Security reports

Use [private GitHub security advisories](https://github.com/jepsenphil/gridstator/security/advisories/new), not a public issue. See [SECURITY.md](SECURITY.md).

By contributing, you license your contribution under Apache-2.0; see [LICENSE](LICENSE).
