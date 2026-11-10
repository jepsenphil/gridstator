# CLI, protocol, discovery, reports and release acceptance

Recorded for the 2026-11-09 upgrade checkpoint. The frozen reference is
`4b8f13ccf1b21820007fef0ab52143c961b69216`. Original accepted-v1 files remain
unchanged. [The evidence index](evidence/cli-protocol-acceptance.json) records the
exact commands, source digests, logs, assertions and limits for 47 ledger rows.

The implementation is frozen for parent integration. The locally executed proof
is Linux x86_64 with Rust 1.99.1 and the locked dependency graph. Actual Mac
snapshots and OS service deployment have separate evidence.

| Ledger IDs | Acceptance |
| --- | --- |
| `cli-help` | All 56 registered help paths and 253 parser processes. 48 help outputs preserve original bytes; eight reflect the approved runtime, native artifact and service metadata changes. Production uses registered command metadata; expected strings exist only in independent test fixtures. |
| `errors-units`, `json-bounds`, `json-defaults`, `project-strictness` | All 45 original schema fixtures, 652 supplementary cases across five schemas, all ten error mappings, optional hints/details, response limits, units, numeric flags and actual untrusted TLS rejection with CLI exit 4. |
| `job-transition-matrix`, `job-reservation-state-membership` | All 81 state transitions and all nine terminal/reservation classifications. |
| `reference-discovery-discovery-001`–`005` | Source workflow, warning, discovery, package manager and Dockerfile assertions, with additional YAML whole-number and Unicode regressions. |
| `reference-reports-junit-001`–`005` | Source nested/direct/single suite assertions, raw/tar/zip extraction and exact shard summaries. |
| `reference-cli-local-controller-001`–`016` | Actual CLI credential/config tests and Linux service-boundary tests. Cases 010–014 control private native controller subprocesses through simulated systemctl, with real TLS, scripted WebSocket workers and a busy listener. |
| `reference-controller-update-compatibility-001`–`007` | Frozen request bodies and error/cause expectations over real loopback HTTP. Indefinite draining is never retried with an omitted deadline. |
| `reference-controller-releases-001`–`007` | Filtering, successful five-minute cache, download order, checksum failure, authorization, failed discovery, invalid tags, staged installation and provisional activation. The native manifest/four-target envelope is the approved artifact substitution. |

The main focused run passed 49 tests; the additional coordination suite passed
five tests in 48.98 seconds. Six CLI unit tests and the release cache/deadline unit
test passed. The release wire suite passed eight tests, including its private
child entry point. Installer/update regression suites also passed. Counts overlap
between commands; they should not be added into a repository-wide unique total.

Supplementary fixtures replay from the independently rebuilt frozen checkout:

```sh
node native/scripts/check-cli-protocol-fixtures.mjs FROZEN_CHECKOUT
```

This compares seven documents as JSON values, preserving exact help, error and
serialized-byte string fields. The native tests do not require Node to consume
these language-neutral fixtures.

The differential work fixed material gaps in short-option ordering, required and
variadic arguments, JavaScript numeric flags and label trimming, task overrides,
Unicode bounds, whole-number JSON/YAML values, controller URL normalization,
error hints/details, TLS error classification, update deadline compatibility,
release caching/error classes and saved local public URLs. The restart harness
also reproduced two source diagnostic failures before their corrections.

Independent review of the workspace regex seam found capture/program cost
amplification in an unweighted backtrack allowance. The behavior owner reproduced
the five-second timeout and added weighted preparation/program/capture/subject
accounting with explicit exhaustion. The final review found no further
high-confidence issue. Its detailed evidence is
[workspace acceptance](evidence/workspace-acceptance.json).

The help comparison covers plain 80-column output, with isolated paths and
host-derived default clauses normalized. It does not prove arbitrary TTY widths
or colored human table output. API schema coverage is limited to the named
schemas and scenarios. Synthetic release payload headers prove verification and
activation state; they are never claimed as functioning cross-platform binaries.
The local coordination suite does not prove an actual systemd or launchd adapter.
Signing, publication and final external target validation remain separate gates.
