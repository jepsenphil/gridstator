# Replacement native development preview

Build `native-preview-20261110-e3e5fae51c8e` supersedes the frozen earlier preview
with the retained-client encoded blob GET fix. This is an uncommitted development
checkpoint, not stable release approval. Embedded source revision is the parent
reference; [source-checkpoint.json](source-checkpoint.json) identifies the actual 40
production/build inputs.

The [quality summary](quality/summary.json) records 242 default native passes, zero
failures and 31 explicit ignored cases across 42 targets, plus strict static checks.
Actual tool/platform tests have their own bounded host evidence; ignored cases are
not counted as successful execution.

The [retained-client regression](../blob-client-encoding-2026-11-10/README.md) includes
the before-fix failure, exact binary-byte TLS checks and actual TypeScript SDK proof.
Four real target binaries and final installation checks are recorded by the
[assembly proof](assembly/README.md). Stable release remains gated on the eight
open platform, service, editor, mixed-update and signing rows.
