# Final preview: clean Linux ARM64 runtime

The exact `native-preview-20261110-6994d2cc804b` Linux ARM64 executable passed actual
unprivileged, offline, read-only Alpine execution in the M4 Docker Desktop ARM Linux VM.
[Receipt](evidence.json), [command](runtime-smoke.sh), [output](runtime-smoke.log) and
[image identity](runtime-image.json) retain the exact artifact identity and constraints.

Native init, authenticated private-CA TLS status and graceful SIGTERM/cleanup passed with
no JavaScript runtime, compiler, Python, SQLite or OpenSSL executable in runtime PATH.
The custom config uses disposable container tmpfs: existing parent permissions remain
unchanged, the config is `0600` and dedicated data is `0700`. No host `/tmp` permissions
were changed. The owned container and runtime credentials were removed automatically.

This proves this final payload's foreground CLI/controller behavior on real ARM Linux.
It does not establish an ARM systemd/boot lifecycle, a complete clean/minimum-OS matrix,
physical Intel behavior or Apple signing. Other workflows have separate receipts.
