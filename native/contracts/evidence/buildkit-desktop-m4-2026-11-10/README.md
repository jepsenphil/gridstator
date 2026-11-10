# M4 Docker Desktop BuildKit metadata acceptance

The actual 14-case executor run on Docker Desktop 4.99.0 passed 12 cases and
exposed two image-ID failures after successful local builds. Captured BuildKit
0.34.2 metadata omits `containerimage.config.digest`; its manifest/index digest
exactly matches the containerd image store's actual `docker image inspect` ID.

The native executor retains classic config-digest precedence. For local `load`
and `tar` only, it falls back to that manifest/index digest. The full actual M4
executor suite then passed all 14 cases, including the strengthened equality
assertion against Docker's actual ID and a positive image size. A portable test
uses the captured metadata, preserves classic precedence, and checks cache-only,
push and missing-metadata boundaries. Strict all-target Clippy passed and an
independent narrow review found no issues.

[evidence.json](evidence.json) records exact commands, source hashes, tool versions
and bounded results. Red output and the raw reproduction are preserved. This
source acceptance uses temporary validation binaries; final release-byte checks
are recorded separately. The replacement preview is
`native-preview-20261110-1fd6e3d425b3`. The earlier e3 preview remains frozen.
