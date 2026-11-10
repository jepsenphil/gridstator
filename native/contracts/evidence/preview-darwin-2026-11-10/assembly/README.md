# Native preview assembly and private installer verification

Classification: Historical unpublished cd6 preview; superseded by the pending inspectable Buildx image-reference correction, not latest delivery. Exact build `native-preview-20261110-cd6b769ebe6e` is in `native/release/native-preview-20261110-cd6b769ebe6e`. The manifest/native-bundle bytes are immutable; all adjacent files are covered by the copied final `release-SHA256SUMS`. `summary.json` records their exact digests and verified file count.

The four real target archives passed independent header, size, executable/archive digest, shared identity, license and native-bundle membership checks. The original source/embedded-input checkpoint was verified at assembly time; these records are bounded to that source snapshot, while later workspace fixes may differ. Linux static linkage was checked directly. Per-target runtime records distinguish actual execution from compilation and retain Rosetta, signing, boot/service and workload limits.

The actual Linux x64 installer/manual verification used only OS tools in the target PATH, with Node/npm/Rust/Python/jq absent. Private init/TLS status/native snapshot/encoded-digest exact byte read-back and graceful stop passed. The final installer then downloaded the completed checksum file exactly, into a private prefix containing spaces; final and nested provenance checksums passed. No global prefix or live service changed. Owned HTTP/controller actors stopped and private credentials/data were removed.

See PREVIEW.json for historical status or remaining release limits. No public release was published.
