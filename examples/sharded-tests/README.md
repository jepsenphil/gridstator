# Example: sharded tests

Eight tests that take 1.5 s each (12 s in one process), split across workers with Node's
`--test-shard`. Gridstator substitutes `{shard}` and `{total}`, spreads the shards over different
workers, and combines their JUnit results.

```bash
cd examples/sharded-tests
gridstator run --task test                    # 2 shards (from gridstator.yml)
gridstator run --task test --shards 4         # override the shard count
gridstator run --shards 3 -- npx vitest run --shard={shard}/{total}    # any framework with sharding
gridstator groups
gridstator group show <group-id>              # per-shard status + combined test results
gridstator group retry <group-id>             # re-run only the shards that failed
```

Frameworks with built-in sharding: Playwright (`--shard={shard}/{total}`), Vitest (`--shard`), Jest
(`--shard`), Node's test runner (`--test-shard`). Every shard also gets `GRIDSTATOR_SHARD_INDEX` and
`GRIDSTATOR_SHARD_TOTAL` for custom splitting.
