Frozen7c configuration-parent permission regressions

Both bounded reproductions passed their expected historical-failure assertions against the immutable7c Linuxx64 executable. All native/reference writes, HOME/XDG/data/cache/runtime/temp paths and working directories were inside fresh owned TemporaryDirectory instances, and every instance was removed afterward. No actual `/tmp` or user directory was a chmod target; no services/listeners were started.

Run from the repository root:

```sh
python3 native/contracts/evidence/config-parent-permissions-2026-11-10/baseline/reproduce.py \
  --binary native/self-build/proof-01M72202QV3TGHQEHTC67VN3HJ/linux-amd64/gridstator \
  --output native/contracts/evidence/config-parent-permissions-2026-11-10/baseline/results.json
```

The driver verifies the frozen executable and source SHA values before running. `results.json` records exact subprocess commands, isolated environment, before/after modes, exit codes and cleanup. Success stdout/config contents are deliberately omitted; no raw token, private key or configuration is retained.

| Case | Immutable native7c | Frozen v1 helper |
| --- | --- | --- |
| Absolute config in existing shared0775 parent | Exit0; parent becomes0700; config0600 | Exit0; parent stays0775; file0600 |
| Relative leaf `GRIDSTATOR_CONFIG=client.json` in owned0775cwd | Exit1/ENOENT; file absent; cwd stays0775 | Exit0; parent stays0775; file0600 |

Native dedicated controller data directories were0700. The root cause is generic file writes calling unconditional private_dir(parent), plus failing to treat an empty relative parent as current directory. Frozen `ensureDir` calls mkdir(recursive,mode0700), which preserves existing directories; writeFileAtomic explicitly keeps the new file0600.

`source-provenance.json` ties the saved pre-fix state.rs bytes to the40-input immutable7c checkpoint and ties the v1 fs.ts bytes to the accepted4b8 baseline. The actual compiled v1 helper is retained as an isolated builtin-only `.mjs` copy, with its SHA in results and SHA256SUMS. This reference uses Node as a test tool and imposes no Node runtime requirement on the native executable.

The approved plan preserves permissions/ownership and private file modes; its four compatibility exceptions do not authorize chmod of arbitrary existing custom-config parents. These are unintended custom-path regressions. Keeping explicit dedicated private_dir enforcement while introducing create-only parent handling and normalizing an empty parent to `.` addresses this boundary. The production fix and its green validation are recorded separately by the parent agent; this baseline remains frozen historical evidence.
