# Native release progress

The native CLI, controller, worker and extension are development previews while stable Linux qualification runs. Automated Rust, TypeScript, VS Code and security checks run in CI. The [release acceptance record](../native/contracts/release-acceptance.json) is authoritative for the two Linux stable gates and three separate future Mac stable gates. Its `implemented` status records partial evidence, not a release pass.

The repository contains [platform evidence](../native/contracts/evidence/) from Linux x64 and ARM64, macOS Apple Silicon and Intel under Rosetta, and WSL2 validation. Historical previews and source revisions in that directory are not evidence for the final 2.0.0 bytes. The [validation host guide](../native/VALIDATION_HOSTS.md) lists the remaining host scenarios and collection steps. The [release guide](RELEASING.md) defines final review and publication.

Current stable priorities are clean runtime and systemd lifecycle on Linux x64 and ARM64 using final candidate bytes. Apple Silicon will be an unsigned development preview; its Docker Desktop and launchd checks continue separately. Intel Mac and signed/notarized macOS distribution are deferred. The desktop companion has a [separate progress record](DESKTOP_COMPANION_PROGRESS.md).
