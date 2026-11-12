# Developing Gridstator

The Rust executable is the CLI, controller and worker. TypeScript remains for the VS Code extension and its shared editor packages. Build and test both parts before submitting changes.

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

The native package is `apps/vscode/dist/gridstator-native.vsix`. It contains the thin extension, webview and installer helper, and contains no bundled CLI. For a real isolated Linux editor/worker run, set `GRIDSTATOR_NATIVE_CLI_E2E` to a built Rust executable and follow [native/TESTING.md](../native/TESTING.md). The webview tests generate ignored fixture screenshots under `apps/vscode/test/ui/screenshots/`. Curated public images and their capture procedure are in [SCREENSHOTS.md](SCREENSHOTS.md).

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
| [CI](../.github/workflows/ci.yml) | Node 22/24 TypeScript checks, VS Code UI/integration and VSIX packaging |
| [Native development](../.github/workflows/native.yml) | Rust tests and actual-host Linux/Mac builds |
| [Desktop preview](../.github/workflows/desktop.yml) | Separate companion build evidence |
| [Security](../.github/workflows/security.yml) | CodeQL, dependency review, pnpm and RustSec audits |
| [Native candidate](../.github/workflows/native-candidate.yml) | Two-target Linux stable candidate or selected unsigned preview, plus thin VSIX; no publication |
| [Native release approval](../.github/workflows/native-publish.yml) | Manual acceptance and protected approval; owner publishes the verified artifact |

Do not interpret green compilation or UI fixtures as completion of platform service, Docker readiness, WSL2, Gatekeeper, recovery or signed downloaded-artifact checks. [Release readiness](RELEASING.md) lists the evidence required.

## Behavior fixtures

The [native behavior fixtures](../native/contracts/behavior/) preserve useful inputs and outputs for Rust acceptance tests. Update a fixture with a focused test and explain why the behavior changed. The `/v1` API route names are protocol versions and remain stable.

## Pull requests

Keep user docs, threat model and tests in the same change as affected behavior. For security-sensitive changes, add a concrete regression scenario. Document any advisory exception with owner, exposure analysis and expiry. See [CONTRIBUTING.md](../CONTRIBUTING.md).
