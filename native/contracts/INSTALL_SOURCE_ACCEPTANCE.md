# Shared install and recovery acceptance

The [evidence index](evidence/install-source-acceptance.json) maps every assertion
in frozen `packages/shared/src/install.test.ts` cases 001–013, plus controller
self-update cases 003–004. It records native tests, approved field substitutions,
commands, results, source digests and separate real-service evidence.

The native replacement keeps argument forwarding, immutable installation,
confirmation, retry/deadline rollback and launcher refresh. It replaces npm package
identity and host Node selection with embedded build metadata, verified native
bytes and a stable native supervisor. Cases 011–013 are not waived merely because
their original fixture selected a Node executable.

The new source suite passed ten tests. Together with installer, release and
existing update regressions, the recorded run passed 20 + 10 + 9 + 10 tests.
Additional actual CLI checks passed for missing-version exit 1 and offline admin
recovery. These command totals overlap and are not a repository-wide unique count.

The final release/refusal suite passed eleven tests after an additional immutable
ID regression: a conflicting bundle using the active ID had returned success and
could fill an empty current-build mirror. Publication, official installation and
bootstrap now compare the verified active manifest before mirror mutation. Honest
identical publication stays idempotent. Both collision paths passed, along with
all seven source release scenarios and self-update refusals. Final all-target
strict Clippy and full formatting checks passed.

The audit reproduced and corrected these retained behavior gaps:

- Unverified launcher environment could replace a worker’s reported build ID and
  could claim controller restart capability outside the installed payload.
- Missing active versions lost the useful version identity in their error.
- Crash-count and missed-confirmation rollback omitted source diagnostics.
- Healthy confirmation did not refresh an older stable native supervisor.

`verify_running_payload()` now verifies embedded identity, physical installed
path, selected target and actual bytes, while reading current state afresh. Its
successful byte cache includes the canonical manifest digest and kernel file
identity/timestamps. Async callers offload filesystem work and retain their
instance leases.

`confirm_running_payload()` keeps the old stable bytes until authenticated role
health. Confirmation and verified atomic replacement share one install lock.
An old provisional payload cannot confirm the candidate. Refresh failure is
reported without undoing a healthy confirmation. Tests cover forged/uninstalled
callers, valid installed and provisional identities, changed manifest invalidation,
idempotent refresh and failed-copy behavior.

Actual current controller/worker roles under real native supervisors are covered
by [managed_role_contract.rs](../tests/managed_role_contract.rs). The earlier
[Linux cutover rehearsal](evidence/linux-cutover-2026-11-10/README.md) separately
records real native activation, embedded-worker rejoin, schema-crash rollback and
database restoration, with exact historical binary/source provenance. Controlled
C children establish argument/state/signal behavior, and do not replace those
actual role/deployment proofs.

The local result is Linux x86_64. Mac snapshots and final overlays have separate
evidence. No signing, publication, reboot or additional target claim is made here.
