# Final Rust v2 development preview

Build `native-preview-20261110-6994d2cc804b`, version `2.0.0-dev.1`.
The CLI/controller/worker are native Rust executables; the thin TypeScript extension
uses VS Code's supplied runtime.

- [Downloads and installation](../../../release/native-preview-20261110-6994d2cc804b/README-preview.md)
- [Independent final verification](final-verification.json): 195 delivery checksums passed.
- [Full quality receipt](quality/summary.json): 250 native tests passed, zero failed,
  32 external-environment cases explicitly ignored; strict Clippy and formatting passed.
- [Assembly and actual installer](assembly/README.md), [clean ARM Linux runtime](arm-clean-runtime/README.md)
  and [source checkpoint](source-checkpoint.json).
- [Progress and five open stable gates](../../../../docs/V2_PROGRESS.md).

Source remains uncommitted; the embedded revision identifies the frozen parent reference,
not a clean native release. The checkpoint is reproduced in its recorded files-array
order (pathlib component sorting), using path + NUL + hexadecimal file SHA-256 + LF.
Native archives, manifest and historical checkpoint documents are preserved unchanged.

This is an unpublished development preview. Physical Intel Mac/runtime coverage, the
complete service/boot matrix and Apple Developer ID signing/notarization remain required.
Actual ARM Linux Docker execution does not establish systemd/boot support. Every historical
receipt retains its original source scope. The existing production v1 cluster was not upgraded; the
[progress record](../../../../docs/V2_PROGRESS.md#test-isolation-incidents) documents two
earlier test-isolation incidents and the strengthened guards.
