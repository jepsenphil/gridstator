# Example: Node.js project

A dependency-free project for trying Gridstator.

```bash
cd examples/node-project
gridstator run --task test          # runs `node --test` on a cluster node
gridstator run -- node --test       # same, without the task
gridstator run --task slow          # Ctrl-C once to cancel the remote job
```

Edit `src/math.js` (no need to commit) and run the tests again: the worker runs exactly the
files on disk, identified by the snapshot ID printed before the job starts.
