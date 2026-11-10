This directory records actual execution on the authorized M4 at `192.168.1.170`: macOS 15.6
(24G84), ARM64, 10 CPUs, 16 GiB RAM. The private validation root is recorded in `cleanup.json`.
Rust 1.99.1, Cargo, rustfmt and Clippy were installed only below that root with
`--no-modify-path`; no global Rust installation, VM, Docker app or fixed Gridstator service was
installed. Apple Command Line Tools, git and Rosetta were already present.

The final tested source is the immutable `99e85b47269b` checkpoint in
`final-source-checkpoint.json`, with source hashes and a distinct embedded build identity.
`summary.json` records 177 default tests passed and 27 explicit opt-in tests ignored on each of
native ARM64 and x86_64 running under Rosetta. Both targets passed strict all-target Clippy.
Both ran actual foreground CLI/controller/worker and unique-label user LaunchAgent
start/restart/stop/uninstall proofs. The new harness is `native/tests/launchd_e2e.rs`.
`final-binary-identities.json` records both thin Mach-O binaries, their hashes, system-only dylib
linkage and signing state. Rosetta execution is not physical Intel Mac evidence. Neither binary
is a Developer ID signed/notarized stable release.

The Mac managed-update rehearsal found a real supervisor race. Darwin returns `EPERM` when
signalling an already-exited, unreaped child's empty process group. The old supervisor treated
that error as fatal and left a downloaded worker pending before launch. The narrow fix checks
and reaps an exited child before signalling, and tolerates a signalling error only when a
second `try_wait` confirms exit; errors for live children remain errors. Owned-session cleanup
still runs. `exited-child-darwin-red.log` reproduces the exact failure;
`exited-child-darwin-green.log`, `exited-child-linux-green.log` and the six Linux supervisor
cleanup/rollback cases verify the fix. The failed update log and durable pending state are
retained separately.

`actual-arm64-managed-update.json` and `managed-update-arm64-green.log` prove the repaired path:
an independently managed standalone worker installs the controller's exact build, persists the
prelaunch state with zero attempts/no deadline, then its same supervisor resumes, starts the
new native payload, rejoins and confirms. The unmanaged worker remains on its original build
and a forced rollout skips it. Both actual host payloads were rebuilt with the supervisor fix,
with distinct `m4-fixed-base-b99b913ff83a` / `m4-fixed-target-b99b913ff83a` identities; their exact
source map is `fixed-effective-source.json`. Other three archive targets in that fixture are
explicit header-only parser fixtures. This does not establish their runtime or release gates.

Test portability corrections preserve production behavior: canonical temp paths are compared
canonically; hostname-derived help defaults normalize their host-dependent wrapping; the CLI
shared-login test expects Darwin's config directory. A source Unicode oracle also keeps the
explicit UTF16 snapshot-path limit distinct from the Zod code-point symlink-target limit.

`cleanup.json` confirms all recorded validation labels/plists are gone, no validation executable
remains running, and controller/agent credential/state directories were removed after exporting
sanitized evidence. Private source/tool/cache/candidate files remain available for later Docker
validation. Docker/BuildKit/act execution, VM capacity/readiness/restart, reboot/logout persistence,
system LaunchDaemon and stable signing/notarization remain outside these results. Docker was
absent and the operator's provisioning choice was still pending during this run.
