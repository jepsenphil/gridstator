# Desktop companion implementation progress

The [confirmed design and acceptance plan](DESKTOP_COMPANION_PLAN.md) is the implementation
baseline. The subsequent “build” request authorizes implementation; no public release or
production-cluster changes are part of this work.

## Implemented preview

| Area | Status |
| --- | --- |
| Native local policy and worker control | Implemented: durable offline Pause/budgets, atomic admission, same-user socket control, masked bounded logs, identity checks and private Join. |
| Independent embedded worker lifecycle | Implemented and tested: Stop/Start preserves controller PID, API and node identity; long drains and durable unacknowledged results supported. |
| Desktop worker window and onboarding | Implemented: jobs/logs, Pause/Resume, graceful/forced Stop, budgets, separate startup settings, Join/Create and WSL/prerequisite recovery. Twelve browser tests pass. |
| Tauri backend, tray and per-user startup | Implemented: single-instance shell, Quit preserves workers, independent worker login helper, trusted native helper discovery/install and private stdin. Linux GUI built and exercised against a real worker. |
| Windows WSL routing and independent host | Implemented: pinned distro/account/worker identity, independent session keepalive, per-user startup and authenticated named pipes. Windows x64 executable cross-compiled; actual Windows lifecycle qualification pending. |
| Packaging and build workflows | Installers are the primary distribution: Windows NSIS setup, macOS DMG and Linux Debian package. Linux x64 Debian and Windows x64 setup artifacts built locally. Verified optional native staging, installer manifests and six-target native-runner CI added; no CI run claimed. |

Linux build-host prerequisites were inspected on Ubuntu 24.04 x64. Required runtime libraries were
already installed; missing development headers were extracted from Ubuntu packages into a private
temporary prefix. No host package installation or user service change was performed.

## Validation evidence

Local build host: Ubuntu 24.04.3 x64, Rust 1.99.1. Desktop version: `2.0.0-dev.1`. Initial GUI smoke
build ID: `desktop-workspace-mwaz5i1m`; installer rebuild ID: `desktop-installers-mwb0mcog`.
Desktop Rust minimum is 1.90; the existing native core minimum is
unchanged. Desktop framework versions are locked in `apps/desktop/src-tauri/Cargo.lock` and
`pnpm-lock.yaml`.

| Check | Result |
| --- | --- |
| Repository TypeScript checking and ESLint | Pass, including the new desktop frontend. |
| Desktop interaction/layout tests | 12 pass; dark/light/narrow captures inspected. IPC is mocked at the browser boundary. |
| Desktop staging/receipt tests | 6 pass; checksums, target mismatch, full native manifest, stale helper removal, correct installer containers and rejection of a portable ZIP/raw executable as the installer. |
| Portable desktop backend | 11 Linux tests pass; join parsing, private transport, corrupt settings, independent startup, persistent Pause and same-session deduplication. |
| Native regression tests | 81 pass across library, local-worker, CLI coordination, service, existing worker, scheduler and frozen differential cases. |
| Real Docker local lifecycle | Both new cases explicitly run and pass: confirmed force-stop and 95-second job drain. Drain test takes 131 seconds and verifies the worker remains alive beyond 90 seconds, finishes the job, preserves offline results and replays them after restart. |
| Rust formatting and Clippy | Pass for native code and full desktop GUI; Windows GUI Clippy includes platform-specific code and the named-pipe authentication test compilation. |
| Linux native GUI smoke | Pass under an isolated X11/WebKit session: real worker discovered, persisted Pause/budgets displayed, actual Resume/Pause clicks mutate that worker, GUI termination leaves it running. No production service/autostart changes. |
| Windows x64 full executable link | Pass using isolated MinGW; this is cross-compilation evidence only. |
| Installer packaging | Windows NSIS wizard with installer hooks and Linux Debian package rebuilt. NSIS archive integrity passes; payload includes the companion, WebView2 loader, install resources and uninstaller. Six packaging tests, scoped ESLint and formatting pass. No installer was executed against the host account. |
| macOS builds/runtime, Windows runtime and ARM64 builds | Not run locally; preview CI configured for native architecture runners. |

The initial broad TypeScript test run had 247 passing tests, 10 skipped and two timeouts in the
unchanged Docker executor suite at its default 20-second threshold. A focused rerun with a
60-second test timeout passed all six executor cases. The full-suite rerun with
`pnpm exec vitest run --testTimeout 60000` passed: **249 tests passed, 10 skipped**, in 34 seconds.
No executor code or configured test thresholds were changed.

Independent reviews caught and resolved cached controller settings being reapplied on reconnect,
an offline policy/startup race, a duplicate Join restarting an owned service, basename-only embedded
worker detection, and Windows named-pipe server impersonation. The Windows client now checks the
connected server process's user SID before transmitting any request bytes. The Windows regression
is compiled here; executing it on Windows remains required.

The Windows preview's unwanted terminal was traced to the debug executable requesting the console
subsystem. All Windows desktop profiles now request the GUI subsystem. The executable-header
regression failed against the previous packaged executable and passed against the rebuilt debug
executable. Windows CI also runs this regression against Cargo's actual debug binary on both
architectures. The Windows installer rebuild uses build ID `desktop-windows-no-console`; actual
Windows launch and lifecycle qualification remain pending.

## Offline Windows setup correction

The Windows installer previously shipped only the download script. Fresh setup consequently failed
with “No matching native release is published yet” because no native release was published.
The existing local native previews also lacked `local-setup-v1` and `worker-control-v1`.

The rebuilt Windows x64 installer now carries both Linux WSL architectures from native build
`native-desktop-20261110-d99c20f05013`. The recorded four-target preview manifest was complete;
Linux payloads were cross-built as static musl executables, and macOS payloads were built and queried
on the existing Mac host (Intel through Rosetta). Linux ARM64 runtime qualification is not claimed.
The companion pins manifest and archive digests in its executable before running any bundled helper.
Setup converts the resource path through the selected distribution's `wslpath`, installs into that
account and retains the verified manifest needed for background-service bootstrap.

Desktop build `desktop-offline-native-d99c20f05013` is at
`distro/2.0.0-dev.1/Gridstator_2.0.0-dev.1_x64-setup.exe` (31,680,270 bytes).
SHA-256: `8c35bb72db0c18bf3a2214e48c3d27903c87d538e2d214cdbd84de3bf3a0eafb`.
It remains an unsigned development preview.

Validation: seven packaging tests, twelve portable backend tests, four native shell-installer tests
and twelve desktop browser tests pass; TypeScript checking, scoped ESLint, formatting and portable
Linux/Windows backend Clippy pass. The installer passes NSIS archive integrity checks and its
embedded executable uses the Windows GUI subsystem. Resources extracted from the final installer
successfully install the real Linux x64 CLI with downloads disabled, retain its verified service
manifest and advertise all required capabilities. Rewritten manifests, archives and adjacent
checksums are rejected against the executable's pinned digests. Actual Windows/WSL execution and
OS lifecycle qualification remain outstanding. [Packaged verification evidence](../native/contracts/evidence/desktop-offline-2026-11-10/README.md)
records the tested scope.

`desktop:installer` now preserves staged native resources and accepts an explicit release directory
and trusted digest. Windows builds reject missing payloads unless a published release tag is
explicitly pinned. Desktop CI first assembles the matching native candidate and bundles its
platform payload; this workflow configuration has not been run here.

## Windows host routing correction

A real Windows process inventory showed an installed GUI at `C:\Gridstator` connected to a
`--worker-host` process from an older Downloads portable preview. The previous endpoint contained
only the account SID, so the new companion accepted the old host and ran its release-download
setup code. Stopping that specific previous host and reopening the installed application allowed
setup to proceed, as confirmed by the user.

Desktop `2.0.0-dev.2` derives its host endpoint from the account, canonical executable path and
SHA-256 of the executable contents. Both the GUI and host independently compute the same identity,
cached once per process. A portable preview at another path or a previous executable from an
in-place update therefore cannot receive the current companion's requests. Existing account SID
authentication and pipe ACLs are retained.

The new portable regression reproduced the collision under the previous implementation, then
passed with the fix. It covers different paths with identical bytes, a changed executable at the
same path, stable identity and account isolation. Thirteen portable backend tests and seven
packaging tests pass, along with TypeScript and portable Linux/Windows backend Clippy. A Windows
named-pipe regression with both preview and installed endpoints is compiled here and configured
to run in Windows CI; it has not been executed on this Linux host. The corrected installer reuses
the verified native archives from the offline setup correction above.

The rebuilt artifact is `distro/2.0.0-dev.2/Gridstator_2.0.0-dev.2_x64-setup.exe`, build
`desktop-host-isolation-dev2`, SHA-256
`37e7ed4231156ba3426eadfba4d291f4f11e0d668ce73a71783afb188c2930d4`.
Its NSIS integrity, compiled routing marker, GUI subsystem, offline archives and digest pins were
checked after extraction. [Artifact evidence](../native/contracts/evidence/desktop-offline-2026-11-10/host-isolation-dev2.json)
records the exact bytes and validation limits. No worker service or startup preference was changed.

## Build artifacts

Installers are also collected at the repository root in **`distro/2.0.0-dev.1/`**, with one checksum
manifest per OS/architecture. Root `desktop:build`/`desktop:installer` commands and CI export to
`distro/<version>/` automatically, retaining other platforms and versions. Each manifest preserves
its own build identity; copying an existing installer does not relabel its source or qualification.

- Linux x64 development package:
  `apps/desktop/src-tauri/target/debug/bundle/deb/Gridstator_2.0.0-dev.1_amd64.deb`.
- Windows x64 per-user setup wizard:
  `apps/desktop/src-tauri/target/x86_64-pc-windows-gnu/debug/bundle/nsis/Gridstator_2.0.0-dev.1_x64-setup.exe`.
  Cross-built with Tauri/NSIS using a privately extracted build toolchain. Includes the WebView2
  loader and bundled native-install resources, Start menu shortcut and registered uninstaller.
  Compiling the installer does not qualify actual Windows install/upgrade/uninstall execution.
- The earlier Windows x64 portable developer preview remains supplementary:
  `apps/desktop/src-tauri/target/preview/Gridstator_2.0.0-dev.1_windows_amd64_preview.zip`.
  Requires WebView2 and WSL2. This ZIP is not an installer or an uninstall experience.
- macOS DMG installers are configured for both native Mac CI runners; no macOS artifact was built
  or tested on this Linux host.
- `installer-manifest.json` alongside each current bundle records the installer format, target,
  exact filename, checksum and source identity. The pipeline rejects missing installers, wrong
  architecture filenames and invalid container headers before upload.
- Separate checksum/source receipts are under `apps/desktop/src-tauri/target/preview/`.
  They deliberately retain `signed: false` and `runtimeValidated: false`: the complete acceptance
  matrix is not qualified by a smoke test.
- Generated browser captures: `apps/desktop/test/screenshots/`. The actual Linux worker capture is
  `real-worker-linux.png`; other captures use browser IPC fixtures.

These are generated, ignored workspace artifacts, not public releases. See
[desktop build instructions](../apps/desktop/README.md) to reproduce packages.

## Remaining release qualification

The implementation is available as a development preview. D0 platform qualification and D6 release
rehearsal remain incomplete; D1–D5 have implementation/test evidence without claiming every OS
acceptance case has passed.

- Run C01–C14 on each advertised OS, including actual sign-out/sign-in/reboot, multiple accounts,
  service-manager integration, upgrades and uninstall. Linux X11 smoke does not qualify Wayland,
  no-tray desktops, ARM64 or all Linux distributions.
- Run W01–W08 on actual Windows/WSL hosts, including NSIS install/upgrade/uninstall, multiple
  distributions, terminal/GUI exit, host helper upgrade and no-display startup. Cross-compiling the
  setup executable does not qualify these cases.
- Run M01–M05 on Intel and Apple Silicon Macs. The configured macOS 11 minimum is a packaging
  setting, not evidence that the actual dependency bundle runs on that version.
- Packages without a bundled native payload still require a compatible published native release
  for automatic downloads. The rebuilt Windows package installs its bundled payload without that
  dependency. No native release was published. Service creation continues to require a verified
  native installation and its matching manifest.
- Complete Windows signing, macOS signing/notarization, downloaded-artifact qualification and
  architecture-specific installer checks. The new CI workflow creates previews and does not mark
  signing or lifecycle validation as passed.
