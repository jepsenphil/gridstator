# Native release readiness

Gridstator 2.0.0 is the first public native Linux release. The [progress report](NATIVE_PROGRESS.md) and [release acceptance record](../native/contracts/release-acceptance.json) track Linux qualification and separate future macOS work.

## Products and channels

| Product | First stable channel | Release rule |
| --- | --- | --- |
| Native CLI/controller/worker | Verified Linux x64/ARM64 archives, bundle, manifest and checksums on GitHub Releases | Both Linux target and systemd gates pass |
| VS Code extension | VSIX release asset first, then `gridstator.gridstator` on the Visual Studio Marketplace | The thin native package matches the Linux candidate and passes extension checks; Marketplace owner setup is required |
| Apple Silicon CLI | Separate unsigned GitHub prerelease | Labeled development preview; manual macOS approval may be needed |
| npm CLI | No package | Native archives and installer are the CLI distribution |
| Desktop companion | Development preview, separate release later | Its OS installer and lifecycle qualification remains open |

The [native candidate workflow](../.github/workflows/native-candidate.yml) builds and checks both Linux targets for stable 2.0.0 and packages a native VSIX. Its `mac-arm` profile produces an unsigned Apple Silicon preview separately. It creates reviewable CI artifacts only. The [manual publish workflow](../.github/workflows/native-publish.yml) checks Linux release acceptance, downloads the exact validated candidate run, verifies checksums and source revision, creates a draft GitHub release, and promotes the release. The first GitHub release includes a downloadable VSIX. The separate [Marketplace workflow](../.github/workflows/marketplace-publish.yml) publishes that VSIX with OIDC after the owner registers the publisher.

## Stable release gate

1. Both Linux stable [release acceptance gates](../native/contracts/release-acceptance.json) have verified final-candidate evidence from actual x64 and ARM64 hosts. `node native/scripts/check-release-acceptance.mjs --release --revision <release-commit> --candidate-revision <validated-candidate-sha>` must pass.
2. Linux amd64/arm64 binaries are static and pass clean-target install/runtime and systemd lifecycle checks. Mac signing, Docker Desktop and launchd gates do not apply to this Linux release and remain open for future stable Mac support.
3. The selected source revision is clean and immutable, with a versioned `CHANGELOG.md` section. Final artifact hashes, manifest identity, VSIX contents and packaged extension integration are checked. The VSIX contains the extension UI and no separate CLI runtime.
4. [CI](../.github/workflows/ci.yml), [native CI](../.github/workflows/native.yml) and [security CI](../.github/workflows/security.yml) pass. High/critical dependency findings and scanner failures are resolved or have an owner-reviewed, time-bounded exception. Verify CodeQL analysis and dependency review reports, not only job names.
5. Clean installation and service install, explicit replacement, restart, stop and uninstall are rehearsed using final candidate bytes. Managed update and rollback have separate contract tests and isolated rehearsal evidence; this first public release has no earlier public Gridstator installation to upgrade. The owner reviews the exact evidence and approves the release.

A green hosted build, cross-compiled ARM executable or browser fixture is not a substitute for required actual-host evidence.

## One-time owner setup

- Register the `gridstator` [Marketplace publisher](https://code.visualstudio.com/api/working-with-extensions/publishing-extension) and reserve the `gridstator.gridstator` extension ID. Configure a **Trusted Publishing** policy for GitHub owner `jepsenphil`, repository `gridstator` and workflow `marketplace-publish.yml`. That job uses `id-token: write` and `vsce publish --oidc`.
- Create the GitHub `v2-release` environment with required reviewers. Require CI, native and security checks on protected main, enable dependency alerts, secret scanning and push protection, and set code scanning merge protection for serious findings where available. The connected repository API cannot read these administrator settings, so verify them in GitHub Settings.
- Activate [GitHub Sponsors for jepsenphil](https://github.com/sponsors/jepsenphil). The [prepared funding file and badge](funding/README.md) should be activated only when the URL resolves to a live Sponsors profile. No dead funding link belongs in a public release.
- Arrange actual Linux x64/ARM64 validation hosts and a separate Apple Silicon host for the unsigned preview. Apple Developer credentials are not required for this Linux release.

[VS Code recommends OIDC trusted publishing](https://code.visualstudio.com/api/working-with-extensions/publishing-extension); its guide says global Azure DevOps PATs retire on December 1, 2026. The publisher policy is an account action, not a repository secret.

## Publication and follow-through

After both Linux gates and owner approval, dispatch **Publish native Linux v2** from `main` with version `2.0.0` and the validated `candidate_run_id`. The release tag points to that candidate's source revision; the evidence commit can follow it only when release-relevant sources are unchanged. Review the GitHub release assets, VSIX, install instructions and checksum verification from clean Linux machines. Register the Marketplace publisher and OIDC policy before dispatching **Publish VS Code extension** for `2.0.0`. The desktop companion follows its [separate qualification plan](DESKTOP_COMPANION_PLAN.md).

