# Atomic config parent permissions and relative filenames

The immutable7c native CLI changed an existing custom config parent's mode from
`0775` to `0700` and failed for a relative leaf config filename. The frozen v1
writer preserved the existing mode and supported relative filenames. The
[baseline receipt](baseline/results.json) reproduces both only inside owned
temporary directories; no actual shared/user directory was changed.

Generic atomic writes and locked JSON updates now create only missing parent
components with `0700` and preserve existing directories. Empty relative parents
normalize to `.` for temporary files, sidecar locks, rename and directory fsync.
Explicit dedicated `private_dir` enforcement remains unchanged. Temporary/config
files and locks remain `0600`; create-new, O_NOFOLLOW, flock and durable rename
semantics remain intact.

Two focused state properties and the whole ten-case CLI contract suite pass.
Absolute and relative CLI cases verify parent0775, file0600 and dedicateddata0700;
a separate state case verifies each newly created parent0700. Strict Clippy and
independent review pass. Red output, commands, patch and source hashes are in
[evidence.json](evidence.json). Final common release-byte proofs are separate
from these source-level debug regression results.
