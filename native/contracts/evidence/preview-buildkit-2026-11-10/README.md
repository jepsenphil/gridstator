# Native development preview with BuildKit compatibility

Build `native-preview-20261110-1fd6e3d425b3` includes both retained-client encoded
blob GET and newer Docker Desktop BuildKit image-ID compatibility fixes. The
[40-input source checkpoint](source-checkpoint.json) identifies the actual native
sources; embedded source revision is the parent reference in this uncommitted
working tree. Earlier previews and their checksum records remain frozen.

The [quality summary](quality/summary.json) records 243 default native passes, zero
failures and 31 explicit ignored cases across 42 targets, strict Clippy, the four
actual Linux Buildx cases, and links to all fourteen actual M4 executor cases.
Actual platform/tool execution has its own bounded evidence. Ignored cases are
not counted as successful execution.

See [assembly/README.md](assembly/README.md) for four real target archives, final
installation/verification and their checksums. This is a private development
preview; eight aggregate platform, service, editor, mixed-update and signing
release gates remain open.
