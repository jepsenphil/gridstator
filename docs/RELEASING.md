# Native release readiness

**Status:** preparation only. Gridstator 2.0.0 is planned as the first public native release. The [progress report](NATIVE_PROGRESS.md) and [release acceptance record](../native/contracts/release-acceptance.json) track open qualification.

## Products and channels

| Product | First stable channel | Release rule |
| --- | --- | --- |
| Native CLI/controller/worker | Four verified Linux/macOS archives, bundle, manifest and checksums on GitHub Releases | All four target and operational gates pass |
| VS Code extension | `gridstator.gridstator` on the Visual Studio Marketplace plus a VSIX release asset | The thin native package matches the same candidate and passes extension checks |
| npm CLI | No package | Native archives and installer are the CLI distribution |
| Desktop companion | Development preview, separate release later | Its OS installer and lifecycle qualification remains open |

The [native candidate workflow](../.github/workflows/native-candidate.yml) builds and checks four actual-host targets, signs/notarizes stable Mac artifacts, assembles a common build identity and packages a native VSIX. It creates reviewable CI artifacts only. The [manual publish workflow](../.github/workflows/native-publish.yml) checks the release acceptance record, rebuilds the candidate, verifies checksums and source revision, creates a draft GitHub release, publishes the VSIX through OIDC, and promotes the release. Do not trigger it before review.

## Stable release gate

1. All five [release acceptance gates](../native/contracts/release-acceptance.json) have verified final-candidate evidence from actual target hosts. `node native/scripts/check-release-acceptance.mjs --release --revision <source-commit>` must pass.
2. Linux amd64/arm64 binaries are static and pass clean-target install and runtime checks. Mac Intel/Apple Silicon artifacts are Developer ID signed, notarized, downloaded, Gatekeeper checked and exercised with Docker/service lifecycle. WSL2, late Docker readiness, mixed-target update and rollback evidence are recorded.
3. The selected source revision is clean and immutable, with a versioned `CHANGELOG.md` section. Final artifact hashes, manifest identity, VSIX contents and packaged extension integration are checked. The VSIX contains the extension UI and no separate CLI runtime.
4. [CI](../.github/workflows/ci.yml), [native CI](../.github/workflows/native.yml) and [security CI](../.github/workflows/security.yml) pass. High/critical dependency findings and scanner failures are resolved or have an owner-reviewed, time-bounded exception. Verify CodeQL analysis and dependency review reports, not only job names.
5. Clean installation, service startup, update and rollback are rehearsed using final candidate bytes. The owner reviews the exact evidence and approves the release.

A green hosted build, cross-compiled ARM executable or browser fixture is not a substitute for required actual-host evidence.

## One-time owner setup

- Register the `gridstator` [Marketplace publisher](https://code.visualstudio.com/api/working-with-extensions/publishing-extension) and reserve the `gridstator.gridstator` extension ID. Configure a **Trusted Publishing** policy for GitHub owner `jepsenphil`, repository `gridstator` and workflow `native-publish.yml`. The publish job uses `id-token: write` and `vsce publish --oidc`.
- Create the GitHub `v2-release` environment with required reviewers. Require CI, native and security checks on protected main, enable dependency alerts, secret scanning and push protection, and set code scanning merge protection for serious findings where available. The connected repository API cannot read these administrator settings, so verify them in GitHub Settings.
- Activate [GitHub Sponsors for jepsenphil](https://github.com/sponsors/jepsenphil). The [prepared funding file and badge](funding/README.md) should be activated only when the URL resolves to a live Sponsors profile. No dead funding link belongs in a public release.
- Arrange Apple signing/notarization credentials and the actual Mac, ARM and WSL2 validation hosts listed in [native/RELEASING.md](../native/RELEASING.md).

[VS Code recommends OIDC trusted publishing](https://code.visualstudio.com/api/working-with-extensions/publishing-extension); its guide says global Azure DevOps PATs retire on December 1, 2026. The publisher policy is an account action, not a repository secret.

## Publication and follow-through

After all gates and owner approval, dispatch **Publish native v2** from `main` with the stable version. Review the draft/release assets, Marketplace listing, install instructions and checksum verification from a clean machine. Update the public docs from preview to released status only after publication. The desktop companion follows its [separate qualification plan](DESKTOP_COMPANION_PLAN.md).

