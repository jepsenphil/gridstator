# M4 host memory, Docker capacity and recovery proof

This is an actual isolated macOS 15.6 M4 run against Docker Desktop 4.99.0 / Engine 29.9.3, using the temporary native ARM64 release `m4-memory-dev-181bd9221ed7`. It overlays only `native/src/worker.rs` on the frozen `native-preview-20261110-1fd6e3d425b3` source checkpoint; it is not a distributed preview bundle. `dev-overlay.json` records the exact source hash; `actual/proof.json` records the actual executable hash and embedded build information. The source revision names the parent commit of the uncommitted preview, rather than claiming a clean commit.

The previous executable delayed a 128 MiB command for 304,825 ms while its sysinfo host-available estimate was below the reserved memory. The first harness timed out; that job later succeeded. `historical/admission-delayed.json` is a later readback, not a recording of the initial zero-memory sample. No claim of permanent job failure is made.

The approved native Darwin estimate is `(free_count + inactive_count) * host_page_size`, with saturating arithmetic and a physical-memory bound. It differs deliberately from legacy Node `os.freemem()` (free pages only) and libuv `uv_get_available_memory()` (free, inactive and purgeable pages). It excludes overlapping speculative/purgeable pages and does not subtract resident compressor pages again. Docker VM memory remains a separate execution-capacity bound; the host estimate is never presented as VM free memory. Both initial node information and recurring heartbeat metrics use the helper; Linux retains its existing sampler.

`sampler/` contains real native and Rosetta C ABI probes, the SDK declaration, nearby `vm_stat`, two passing Rust sampler tests per architecture, ARM strict all-target Clippy, and release build output. Both C executions report a 16,384-byte host page size. macOS 15.6 returns 38 integer words from `host_statistics64` although the newer SDK/libc structure has 40; the implementation checks the three prefix fields it actually consumes. Checked API failures fall back to clamped sysinfo values. One cached Mach host send right is retained for the process lifetime, rather than acquiring a new right on each heartbeat.

The real CLI/controller/standalone worker proof asserted:

- Physical host: 10 CPUs / 16,384 MiB; Docker Linux ARM VM: 10 CPUs / 8,319,438,848 bytes (7,934 MiB); allocatable: 9 CPUs / 6,296 MiB. Initial host available was 4,502 MiB.
- Snapshot capture from a private path with spaces, exact command logs, exact downloaded tar artifact bytes, and cancellation of a running Docker job.
- A real Docker build with local load output. Reported image ID and size (9,430 bytes) exactly matched `docker image inspect`; the uniquely owned image tag was removed.
- A real Docker Desktop stop/start with no other containers present. The same worker PID and websocket `connectedAt` remained online, heartbeat time advanced, executors became unavailable, submitted work stayed queued, and the queued command completed after Docker readiness reprobe. Desktop was left running.

`cleanup.json` verifies that only the private proof actors were stopped, their credentials/state/workspaces removed, and no owned containers or volumes remained. Existing desktop/editor state and unrelated services were untouched. This covers the actual temporary ARM executable; Intel-specific Docker execution and physical Intel hardware are not claimed.

Primary references: [libuv Darwin memory APIs](https://github.com/libuv/libuv/blob/v1.52.1/src/unix/darwin.c), [Apple VM statistics field definitions](https://github.com/apple-oss-distributions/xnu/blob/main/osfmk/mach/vm_statistics.h). SDK declarations captured on the actual host are included here.

Independent review calculated 5,053.4375 MiB from the native C probe and 5,049.28125 MiB under Rosetta, versus 5,056.765625 MiB from the nearby `vm_stat` sample after counting its separately displayed speculative pages once. The 3.33 / 7.48 MiB differences are consistent with separate live samples; a 4x Rosetta page-size error was excluded. `independent-review.json` records the bounded verdict.
