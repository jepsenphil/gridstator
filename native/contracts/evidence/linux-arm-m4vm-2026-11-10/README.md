# Actual Linux ARM64 clean runtime on the M4 Docker VM

The final `native-preview-20261110-cd6b769ebe6e` Linux ARM64 static payload executed
on the M4's actual `aarch64` Linux Docker VM kernel. An unprivileged, offline,
read-only Alpine container had no Node, npm, Rust, Python, Zig, SQLite or OpenSSL
executables. The payload reported its expected hash/target/build, initialized a
private cluster, served a real authenticated TLS status request, handled SIGTERM
and removed its runtime file before the disposable container exited.

[evidence.json](evidence.json), [runtime.log](runtime.log), [runtime.sh](runtime.sh)
and the image metadata record exact identity and bounds. This establishes actual
ARM64 CLI/controller execution rather than only cross-build headers. It does not
establish a standalone Linux ARM64 boot/systemd lifecycle or ARM64 Docker worker
execution.

Initial anonymous pulls hit a locked Mac credential helper. A private Docker
config and OS-only PATH with an absolute Docker CLI avoided automatic helper
detection; no global configuration or keychain was changed. Those red logs and
the passing preflight remain alongside the final runtime proof.
