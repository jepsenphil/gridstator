# Native backend window component acceptance

`models-final.log` records all 10 cases passing in 37.16 seconds using an immutable private CLI copy. Seven ledger scenario mappings (the ready/expired scenario has two cases) are listed in `acceptance.json`; two additional cases verify the isolation fence. `source-and-cli.json` records exact source SHA256 values and the actual CLI identity/digest. Real baseline/confirmed native payload manifests and their original source evidence are linked in the report.

The source scenarios are retained unchanged in `state.test.ts` and `settings.test.ts`; the additional opt-in file is `apps/vscode/src/native-windows.integration.test.ts`. It runs three component model instances in one process with real native REST/TLS/WebSockets, shared-login watching, and actual NativeCli administration. Its protocol peer only models assignment/running state, matching the originating tests. These results do not claim actual SSH/editor windows, native worker execution, Docker execution, Mac validation or stable release signing. The slow restart case uploads a real native bundle, deliberately holds the private supervisor through more than 1.1 seconds of backend downtime, observes retained restoration state, and verifies the actual replacement build, PID, managed health confirmation and restored scheduling.

Earlier failures are retained and described explicitly in the report. The test-isolation incident and reviewed fail-closed guard are documented separately in `../native-window-isolation-2026-11-10`; no subsequent run could use a default or existing login URL. Final fixture cleanup stops only each owned controller/supervisor, disposes watchers/model actions, and removes its private credentials/data.

Run with real Linux host packages (the prerequisite is deliberate and never silently skipped when opted in):

```sh
GRIDSTATOR_NATIVE_WINDOWS_E2E=/absolute/path/to/native/gridstator \
GRIDSTATOR_NATIVE_WINDOWS_RELEASES=/absolute/path/to/rehearsal-packages \
pnpm exec vitest run apps/vscode/src/native-windows.integration.test.ts --no-file-parallelism
```

Without the opt-in executable, only the always-on rejection regression runs; the native integration scenarios are explicitly skipped. The baseline and confirmed directories must contain the actual immutable host payloads and verified bundles, as prepared by the Linux recovery rehearsal.
