# Security model

Gridstator executes developer-supplied code on trusted worker machines. A controller receives source snapshots, job secrets, logs and artifacts. Treat controller data, worker accounts and Docker access as sensitive.

## Supported environment

Native v2 targets a trusted personal or small-team LAN. It is not a multi-tenant execution service or an internet-facing controller. Anyone who can submit a job can cause code to run on workers, subject to the permissions of the worker account and Docker engine. Container limits reduce accidental resource exhaustion; Docker is not a security boundary for hostile users.

The [native release acceptance record](../native/contracts/release-acceptance.json) tracks platform and security evidence. Until its required gates pass, the product is a development preview.

## Boundaries and controls

| Boundary | Control |
| --- | --- |
| Client or worker to controller | TLS with a self-generated cluster CA, fingerprint pinning, named tokens, role checks and revocation |
| Worker network | Worker dials outbound; no inbound worker port is required |
| Snapshot upload | Content digests, manifest checks, path and archive validation, size bounds and safe materialization |
| Job execution | Docker resource limits, ownership labels, cleanup and no privileged container mode by default |
| Release and update | Per-platform checksums, manifest identity, signed/notarized Mac release gate, staged activation and rollback |

The controller is the source of truth for job state. Workers report completion and replay durable results after reconnecting. A controller administrator can issue or revoke credentials and publish verified builds; restrict that role to trusted maintainers.

## Source and secrets

Snapshots include saved tracked and untracked files while respecting project ignore rules. Common secret files such as `.env` and private keys are excluded by default, but a user can still put secrets in other files or deliberately allow them. Review `gridstator snapshot --dry-run` before sending a sensitive tree.

A job's `secretEnv` names are resolved on the client. The CLI reads values from its shell environment. In a trusted VS Code workspace, the extension reads its workspace-host environment or project-scoped VS Code SecretStorage and asks for missing values with masked prompts. Values are not stored in workspace settings, webview state or the snapshot. Plain environment fields must not duplicate configured secret names. The extension's **Clear Saved Job Secrets** command removes selected saved values. The workspace-host account and VS Code manage local secret storage; values do not sync to another host.

Once submitted, job secrets are retained by the controller so retries can reproduce the run. They are redacted from streamed logs and API responses, but the controller's database and backups must be protected. Keep admin tokens and join commands out of shared logs, screenshots and issue reports.

## Supply chain and pull requests

[CI](../.github/workflows/ci.yml) builds and tests the extension; [native CI](../.github/workflows/native.yml) covers actual Linux x64/ARM64 and Apple Silicon hosts. [Security CI](../.github/workflows/security.yml) scans Rust, JavaScript/TypeScript and Actions with CodeQL, reviews changed dependencies, and audits the Node and Rust lockfiles. High and critical dependency findings fail PR checks. Scanner failures also fail. Any exception must identify the advisory, affected version, exposure analysis, owner and expiry in the PR or a tracked issue; do not silently suppress a scanner. [Dependabot](../.github/dependabot.yml) covers npm, Cargo and Actions.

The 2026-11-12 lockfile triage upgraded direct `quick-xml` and `zip` dependencies to clear high/critical RustSec findings. The remaining advisory is a moderate `minimatch` issue in the development-only ESLint dependency; the configured high/critical threshold does not suppress its report. RustSec also warns that `rustls-pemfile` and `unsafe-libyaml` are unmaintained. Review these findings at each dependency update and before each release.

Repository administrators should enable dependency alerts, secret scanning, push protection and required PR checks. These settings are outside the source tree; the [release checklist](RELEASING.md) records how to verify them.

The protected [v2 release approval workflow](../.github/workflows/native-publish.yml) is manual and gated by native release acceptance evidence. The owner creates the GitHub release from its approved artifact; no release PAT is stored in Actions. Marketplace publishing uses GitHub OIDC trusted publishing. The owner must register the `gridstator` publisher and its workflow trust policy before publication. No long-lived Marketplace PAT is needed.

## Report a vulnerability

Use [GitHub's private security advisory form](https://github.com/jepsenphil/gridstator/security/advisories/new), not a public issue. Include affected version, configuration and a minimal reproduction. The top-level [security policy](../SECURITY.md) states response and support terms.
