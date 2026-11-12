# Developing Gridstator

Native v2 is the current implementation. The TypeScript CLI/controller/worker under `apps/` and `packages/` remain a v1 reference and compatibility test bed. Build and test the two lines separately; a passing v1 test does not close a native release gate.

## Prerequisites

Use the pinned toolchain in [native/rust-toolchain.toml](../native/rust-toolchain.toml), a C compiler, Node 24, pnpm 10.33.2, Git and Docker. Buildx and `act` are needed for their execution suites. Linux VS Code integration needs Xvfb. Installed native users need neither Rust nor Node; Node is used to build and test the extension.

~~~sh
pnpm install --frozen-lockfile
cd native
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets -- --test-threads=1
cargo build --locked --release
./target/release/gridstator build-info --json
~~~

The full real-tool and platform matrix is in [native/TESTING.md](../native/TESTING.md). Ignored tests and cross-compilation are not runtime evidence.

## VS Code extension

~~~sh
pnpm build
pnpm typecheck
pnpm lint
pnpm test
pnpm --filter ./apps/vscode package:native
pnpm test:ui
~~~

The native package is `apps/vscode/dist/gridstator-native.vsix`. It contains the thin extension, webview and installer helper, and excludes the v1 CLI. For a real isolated Linux editor/worker run, set `GRIDSTATOR_NATIVE_CLI_E2E` to a built Rust executable and follow [native/TESTING.md](../native/TESTING.md). The webview tests generate ignored fixture screenshots under `apps/vscode/test/ui/screenshots/`. Curated public images and their capture procedure are in [SCREENSHOTS.md](SCREENSHOTS.md).

## Run this repository through Gridstator

The root [gridstator.yml](../gridstator.yml) exposes daily tasks in the VS Code Run view:

| Task | Purpose |
| --- | --- |
| `native-check` | Rust format, Clippy and serial tests |
| `vscode-check` | TypeScript build, checks, tests and native VSIX package |
| `rust-audit` | High/critical RustSec lockfile audit |
| `js-audit` | High/critical pnpm lockfile audit |
| `native-linux` and target variants | Static Linux build artifacts; separate from actual ARM runtime qualification |

Run `gridstator run --task native-check` from the repository root or click the task in VS Code. These are worker jobs and require a configured trusted development cluster. Build and security CI run independently on pull requests; no contributor needs a Gridstator cluster to submit a PR.

## CI and release evidence

| Workflow | Scope |
| --- | --- |
| [CI](../.github/workflows/ci.yml) | Node 22/24, package smoke, Docker/BuildKit/act and VS Code integration |
| [Native development](../.github/workflows/native.yml) | Rust reference contracts and actual-host Linux/Mac builds |
| [Desktop preview](../.github/workflows/desktop.yml) | Separate companion build evidence |
| [Security](../.github/workflows/security.yml) | CodeQL, dependency review, pnpm and RustSec audits |
| [Native candidate](../.github/workflows/native-candidate.yml) | Four-target signed candidate and thin VSIX; no publication |
| [Native publish](../.github/workflows/native-publish.yml) | Manual stable release after ledger and owner gates |

Do not interpret green compilation or UI fixtures as completion of platform service, Docker readiness, WSL2, Gatekeeper, recovery or signed downloaded-artifact checks. [Release readiness](RELEASING.md) lists the evidence required.

## Working on v1 compatibility

The accepted v1 source and fixtures are frozen in [native/contracts/accepted-v1](../native/contracts/accepted-v1). Use the [legacy development guide](legacy/v1/DEVELOPMENT.md) only for v1 reference work. If a v1 behavior changes during native development, review its effect on the parity ledger and compatibility exceptions. Do not regenerate expectations from the v2 implementation.

## Pull requests

Keep user docs, threat model and tests in the same change as affected behavior. For security-sensitive changes, add a concrete regression scenario. Document any advisory exception with owner, exposure analysis and expiry. See [CONTRIBUTING.md](../CONTRIBUTING.md).
