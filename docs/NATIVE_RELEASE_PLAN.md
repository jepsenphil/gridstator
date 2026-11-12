# Native release plan

Gridstator 2.0.0 is the planned first public release of the Rust CLI, controller, worker and VS Code extension. The desktop companion follows a separate qualification and release. The CLI is distributed as verified native archives and an installer; there is no npm CLI package.

## Release acceptance

The machine-readable [release acceptance record](../native/contracts/release-acceptance.json) has five remaining gates: Mac signing and notarization, four-target clean-host linkage and runtime, Mac Docker capacity and readiness, launchd lifecycle, and systemd lifecycle. Each gate needs candidate bytes, source revision, commands, actual host identity and results. `node native/scripts/check-release-acceptance.mjs --release --revision <source-commit>` requires passing proof from an ancestor with the same release-relevant source; the candidate workflow and owner review check the final signed bytes. A development build or simulated platform test does not close a live-host gate.

The [native candidate workflow](../.github/workflows/native-candidate.yml) builds four targets and a VSIX for review. The [release guide](RELEASING.md) specifies signing, checksums, security checks, Marketplace setup and owner review. The [validation host guide](../native/VALIDATION_HOSTS.md) contains concrete platform procedures. No workflow run or repository change alone authorizes publication or a live cluster change.

## Distribution and support

The release identity stays 2.0.0 because it is already embedded in tested native install and update paths. The public docs describe it as the **first public native release**. The extension's permanent Marketplace ID is `gridstator.gridstator`. Every VSIX delegates workspace operations to the Rust executable on the workspace host; VS Code itself supplies the extension's JavaScript runtime. The desktop companion remains a development preview until its own platform gates pass.

No earlier product generation needs migration or recovery. Internal `/v1` routes and capability strings version the wire protocol and remain unchanged.
