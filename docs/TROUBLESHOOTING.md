# Troubleshooting native v2

Start with `gridstator doctor` on the source host and each worker. Record `gridstator build-info --json`, the OS/architecture, the command, and whether the controller and worker are running. Never paste tokens, private keys or unredacted logs into a public issue.

## Cannot connect

- Check the controller service and `gridstator status` on its host. Confirm its configured listening address is reachable from the client/worker.
- Confirm the URL and pinned CA fingerprint match the intended cluster.
- Check the token role, expiry and revocation state. Restore local access from the owning controller account if a local token was revoked.
- In Remote SSH/WSL, run `doctor` on the **workspace host**, where the extension and helper actually run.

## Worker offline or jobs queued

- Run `gridstator nodes` and `gridstator doctor` on the worker. Check its user service, outbound network path and Docker access.
- Docker or Docker Desktop may start after the worker. Check actual Docker capacity, reserved CPU/RAM and concurrency limits; the scheduler may correctly leave a job queued.
- Verify the requested worker, labels, image, architecture and resources are available. A draining or disabled worker will not accept new jobs.

## Build or Actions failure

- For image builds, verify `docker buildx version` on the worker and inspect the job's BuildKit output.
- For GitHub Actions jobs, verify `act --version` and Git on the worker. The local `act` executor supports a subset of GitHub-hosted behavior; see the job's compatibility message.
- Download artifacts and inspect logs with `gridstator job <id>`, `gridstator logs <id>` and `gridstator artifacts <id>`.

## VS Code

- The native VSIX needs a compatible Rust CLI on the extension host. Set `gridstator.nativeCli.path` to an absolute path if discovery fails.
- Trust the workspace before running a job. Save editor buffers; snapshots use saved files.
- Missing `secretEnv` values are requested through masked prompts and saved in VS Code SecretStorage. Use **Gridstator: Clear Saved Job Secrets** to replace a saved value. An interactive shell variable may not exist in the VS Code extension host environment.
- The first-run automatic installer requires a **published verified native release**. During development, build/install the CLI manually.

## Updating and recovery

Do not manually edit a release store state file. Keep the exact candidate bundle, manifest/checksums and supervisor logs when diagnosing an update. See the [native release procedure](../native/RELEASING.md) for install and rollback validation.
