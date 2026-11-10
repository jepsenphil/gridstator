# Linux clean install, cutover and recovery evidence

Observed on Ubuntu 24.04.3 x86_64 with the existing user systemd manager. The isolated rehearsal
passed one opt-in test in 103.40 seconds on 2026-11-10:

```sh
GRIDSTATOR_KEEP_RECOVERY_EVIDENCE=1 \
GRIDSTATOR_RECOVERY_SYSTEMD=1 \
GRIDSTATOR_RECOVERY_PREBUILT=/tmp/gridstator-recovery-sxVNiw \
  /home/pj/.cargo/bin/cargo +1.99.1 test --manifest-path native/Cargo.toml \
  --test recovery_e2e -- --ignored --nocapture
```

`rehearsal.json` records actual schema/history facts before, after the failed candidate and after
restoration. `supervisor.log` is the actual owned unit journal; `installer.log` and the two v1 logs
are actual subprocess output. The service used a unique name and was stopped/removed by the test.

Three real static Linux executables were built in an earlier disposable source copy and reused
for this execution, as the report explicitly records. Per-build build-info and source SHA-256
manifests identify those exact payloads; they do not claim subsequent shared workspace edits were
in those binaries. The schema-crashing payload is an actual executable that changes the SQLite
schema/data before exiting. Other-target release members are parsing fixtures, without runtime
coverage. The v1 archive/restoration used the actual retained Node CLI and a private old cluster.

The installer ran with ordinary OS tools on a private PATH without Node/npm/Rust/Python/jq. The
test also verified a manual installation, distinct fresh native CA/cluster ID, rejection of old
credentials/trust, a successful authenticated native controller update, embedded-worker rejoin,
crash rollback, schema/history restoration and separately retained failed candidate DB facts.
This does not establish other-target, WSL2, mixed-target LAN, signing, system-service or reboot
acceptance. See [CUTOVER.md](../../../CUTOVER.md) and
[recovery_e2e.rs](../../../tests/recovery_e2e.rs) for repetition and assertions.

`SHA256SUMS` covers the copied reports/logs and identity files. Credential-like `gs[acj]_` tokens
were absent from these files before copying. Databases, CA/private keys, client/agent credentials,
archives containing credentials, binaries and build caches are deliberately excluded. Original
private working evidence was retained at `/tmp/gridstator-recovery-arhXMg` when this copy was made.
