# Actual M4 Docker worker scenarios

Eight explicitly ignored Docker worker cases passed on the native Apple Silicon
Mac after the reviewed Darwin memory fix. These cover real SIGTERM/drain,
coordinator cancellation, durable lost results, missing snapshots, artifact
failures, disconnected completion/log replay and update-admission handoff.

[worker-suite.log](worker-suite.log) and [evidence.json](evidence.json) record the
command, result and bounds. The compiled test uses a temporary development ID; its
production sources match the final cd6 checkpoint. The independent managed-worker
package-fixture case is excluded and has separate acceptance evidence. Physical
Intel, system services and reboot are not claimed.
