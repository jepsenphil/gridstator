# Native release plan

Gridstator 2.0.0 is the planned first public stable release of the Rust CLI, controller, worker and VS Code extension **on Linux x64 and ARM64**. An unsigned Apple Silicon build will use a separate prerelease tag and will be labeled a development preview. Intel Mac and the desktop companion are not part of the stable release. The CLI is distributed as verified native archives and an installer; there is no npm CLI package.

## Release acceptance

The machine-readable [release acceptance record](../native/contracts/release-acceptance.json) separates two Linux stable gates from three future macOS stable gates. Linux needs clean runtime on both actual architectures and real systemd lifecycle evidence on both architectures. Each gate needs candidate bytes, source revision, commands, actual host identity and results. `node native/scripts/check-release-acceptance.mjs --release --revision <release-commit> --candidate-revision <validated-candidate-sha>` requires passing proof from that exact candidate; release-relevant sources must be unchanged between the candidate and evidence commits. A build or simulated platform test alone does not close a live-host gate. Mac signing, Docker Desktop capacity and launchd remain open for any future stable macOS release.

The [native candidate workflow](../.github/workflows/native-candidate.yml) builds two Linux targets for stable 2.0.0, or selected preview targets, plus a VSIX. The [release guide](RELEASING.md) specifies checksums, security checks, Marketplace setup and owner review. The [validation host guide](../native/VALIDATION_HOSTS.md) contains concrete platform procedures. No workflow run or repository change alone authorizes a live cluster change.

## Distribution and support

The release identity stays 2.0.0 because it is already embedded in tested native install and update paths. The public docs describe it as the **first public native Linux release**. The extension's permanent Marketplace ID is `gridstator.gridstator`. Every VSIX delegates workspace operations to the Rust executable on the workspace host; VS Code itself supplies the extension's JavaScript runtime. The desktop companion remains a development preview until its own platform gates pass.

No earlier product generation needs migration or recovery. Internal `/v1` routes and capability strings version the wire protocol and remain unchanged.
