# Native release progress

The native CLI, controller, worker and extension are development previews. Automated Rust, TypeScript, VS Code and security checks run in CI. The [release acceptance record](../native/contracts/release-acceptance.json) is authoritative for the five open platform, service and signing gates. Its `implemented` status records partial evidence, not a release pass.

The repository contains [platform evidence](../native/contracts/evidence/) from Linux x64 and ARM64, macOS Apple Silicon and Intel under Rosetta, and WSL2 validation. Historical previews and source revisions in that directory are not evidence for the final 2.0.0 bytes. The [validation host guide](../native/VALIDATION_HOSTS.md) lists the remaining host scenarios and collection steps. The [release guide](RELEASING.md) defines final review and publication.

Current priorities are signed and notarized Mac candidates, clean target runtime on all four architectures, real Docker VM capacity and late-start checks on Macs, launchd lifecycle on both Mac architectures, and systemd lifecycle on Linux x64 and ARM64. The desktop companion has a [separate progress record](DESKTOP_COMPANION_PROGRESS.md).
