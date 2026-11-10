# Worker rename verification

Native feature preview: `native-rename-20261110-f0b1636fbd91`, version `2.0.0-dev.2`.

`verification.json` records the verified native bundle, installed extension hashes, managed rollout,
and the live rename of `DESKTOP-JEL` to `big-box`. The worker retains ID
`01M73H8CW57VX01DZXK5KCAZFP`, its machine identity, settings and uninterrupted connection. A tiny
snapshot-free Docker job targeted the new name, succeeded, and printed `big-box` from
`GRIDSTATOR_NODE_NAME`. No sibling staging publish was performed.

Validation logs record 313 passing TypeScript tests and 10 skipped, 26 native controller/CLI
contracts, two native name-persistence unit cases, strict native Clippy and 29 browser cases.
Focused TypeScript coverage includes duplicate/invalid names, atomic mixed patches, queued and
retried job references, worker persistence, old-controller no-op detection, and socket replacement
while a name write is pending. An independent review caught and fixed stale queued WebSocket
frames applying to a replacement socket; the real TLS/WebSocket regression now proves they are
discarded. A pre-existing toolchain-test readiness race was tightened to await both tool gaps,
retaining its original assertions.

The four `worker-rename-*.png` captures use the existing browser harness's labeled fixture state,
not live cluster screenshots. Wide/narrow panels in dark/light themes were captured and opened.
The added name field uses the existing worker-card settings form and sends a name-only PATCH
when resource settings have not changed. Draft names survive status polling.

The complete four-target release manifest remains intact. Linux x64 and the WSL x64 worker were
executed; Linux ARM64 was cross-compiled. macOS ARM64 and Intel payloads were built on the Mac
host, with Intel build-info executed under Rosetta. Stable platform, signing and OS lifecycle
qualification are not claimed. The source snapshot and immutable artifact hashes identify this
uncommitted development build; no public release was published.
