# Retained-client encoded blob GET acceptance

A real editor dirty-buffer read-back exposed a missing public-client URI boundary.
The retained TypeScript client percent-encodes the blob digest colon, while the early
native GET handler validated the raw URI suffix. The recorded red test returns 404.
The handler now uses the existing safe decoder once, then validates the digest.

Six native controller contracts pass, including exact binary/NUL/non-UTF8 read-back
through TLS for raw, upper-case and lower-case escaped colons. Double encoding and
encoded traversal return 404; malformed escapes return 400. The actual retained
TypeScript SDK also reads exact native snapshot bytes in an isolated TLS lifecycle.
Commands, logs, source hashes and copied executable identity are in [evidence.json](evidence.json).

The shared replacement preview is `native-preview-20261110-e3e5fae51c8e`. Its
40 production/build inputs are recorded in [source-checkpoint.json](source-checkpoint.json).
The previous preview and its checksums remain frozen as historical evidence.
This is bounded interoperability proof, not stable release approval.
