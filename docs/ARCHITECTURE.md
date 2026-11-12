# Native v2 architecture

This is the architecture of the Rust v2 development line. The [v1 Node design](legacy/v1/ARCHITECTURE.md) is retained for recovery and parity work. Current evidence and unclosed gates are in [V2_PROGRESS.md](V2_PROGRESS.md) and the [parity ledger](../native/contracts/parity-ledger.json).

## System at a glance

~~~mermaid
flowchart LR
    subgraph Source["Developer workspace host"]
      VS["VS Code extension<br/>UI + native bridge"]
      CLI["gridstator CLI"]
      CAP["Saved-file snapshot capture"]
      VS --> CAP
      CLI --> CAP
    end
    subgraph Control["Controller host"]
      API["TLS API + event stream"]
      Q["Scheduler + job state"]
      DB[("SQLite + logs")]
      CAS[("Content-addressed blobs")]
      API --> Q
      Q --> DB
      API --> CAS
    end
    subgraph Worker["Worker host"]
      AG["Native worker<br/>outbound connection"]
      RUN["Command / BuildKit / act"]
      DOCKER["Docker engine"]
      AG --> RUN --> DOCKER
    end
    CAP -->|"HTTPS: manifests, missing blobs, jobs"| API
    API -->|"WSS: events"| VS
    AG -->|"WSS: registration, assignment, results"| API
    AG -->|"HTTPS: missing blobs"| CAS
~~~

The native executable contains CLI, controller, worker, snapshot, scheduling and execution code. The extension's TypeScript host and React webview remain separate because VS Code supplies the editor runtime. In a Remote SSH or WSL window, the extension and native helper run on the workspace host; the UI renders on the user's desktop. The [desktop companion](../apps/desktop/README.md) is a separate preview for local worker control.

## Repository map

| Path | Native v2 responsibility |
| --- | --- |
| [native/src/cli.rs](../native/src/cli.rs) and [client.rs](../native/src/client.rs) | Commands, local configuration, authenticated API client |
| [native/src/bridge.rs](../native/src/bridge.rs) and [apps/vscode](../apps/vscode) | Editor/native boundary, VS Code views and webview |
| [native/src/workspace.rs](../native/src/workspace.rs) and [workspace/](../native/src/workspace) | Working-tree selection, snapshots, CAS and safe materialization |
| [native/src/controller.rs](../native/src/controller.rs), [db.rs](../native/src/db.rs) and [scheduler.rs](../native/src/scheduler.rs) | TLS API, durable state, queue, placement and reconciliation |
| [native/src/worker.rs](../native/src/worker.rs) and [executor/](../native/src/executor) | Outbound worker, Docker commands, image builds and supported Actions |
| [native/src/updates.rs](../native/src/updates.rs), [release.rs](../native/src/release.rs) and [install.rs](../native/src/install.rs) | Artifact verification, installation, updates and rollback |
| [native/src/protocol.rs](../native/src/protocol.rs) | Shared wire contracts and version checks |

The TypeScript packages and Node CLI under `apps/cli` are the frozen v1 reference. They are not the runtime for a native v2 cluster.

## Job and data flow

~~~mermaid
sequenceDiagram
    participant C as CLI / VS Code helper
    participant K as Controller
    participant W as Worker
    participant D as Docker / BuildKit / act
    C->>C: Capture saved files, apply ignore and secret rules
    C->>K: Ask which content digests are missing
    C->>K: Upload missing blobs and manifest
    C->>K: Submit job referencing immutable snapshot
    K->>K: Validate, persist, queue and select worker
    K-->>W: Assign job over outbound worker channel
    W->>K: Fetch missing snapshot content
    W->>D: Materialize workspace and run
    D-->>W: Logs, steps, artifacts and exit status
    W-->>K: Durable result and replayable output
    K-->>C: Events, logs, JUnit and artifacts
~~~

The controller owns job transitions, tokens and scheduling. Workers report facts and can reconnect after a lost channel. A worker stores completion data for replay until the controller acknowledges it. Snapshots are addressed by content digest so repeat runs transfer only changed files. Jobs execute the **saved** files on disk; the extension asks about unsaved editors before launch. See [workspace code](../native/src/workspace.rs), [state code](../native/src/state.rs) and [worker code](../native/src/worker.rs).

## Persistence and lifecycle

The controller stores jobs, nodes, tokens, settings and event state in SQLite, plus content-addressed blobs and logs on disk. Workers maintain local identity, a blob cache and a durable result outbox. A worker initiates the network connection to the controller, so worker hosts need no inbound listening port. The controller can run an embedded worker on the same machine.

A verified native release has one build identity and platform-specific checksums. A managed update stages the new executable, drains the affected role, backs up controller state and waits for a health confirmation. An unhealthy candidate rolls back to the previous executable and database. These rules are described and rehearsed in [native/RELEASING.md](../native/RELEASING.md) and [native/CUTOVER.md](../native/CUTOVER.md); platform evidence is tracked separately from passing unit tests.

## Trust boundaries

~~~mermaid
flowchart LR
    Dev["Trusted developer + workspace"] -->|"pinned TLS + client token"| Ctrl["Controller<br/>CA, tokens, state, source blobs"]
    Worker["Trusted worker + Docker access"] -->|"outbound pinned TLS + worker identity"| Ctrl
    Ctrl -->|"authorized job assignment"| Worker
    Worker -->|"runs user project code"| Docker["Containers / BuildKit / act"]
~~~

Gridstator is for a trusted personal or small-team network. Docker resource limits protect host availability but do not turn a worker into a safe multi-tenant sandbox. The controller and its data directory are high-trust assets: snapshots can contain source, logs can contain output, and job secrets are retained for retries. The extension resolves configured job secret names from the host environment or project-scoped VS Code SecretStorage and prompts for missing values in trusted workspaces. See [security details](SECURITY.md).

## Protocol and compatibility

The v2 implementation keeps the `/v1/...` HTTP and WebSocket route family for the accepted wire contract; the route prefix is not the product release number. Native clients and workers perform capability/version checks. Do not mix v1 and v2 credentials, databases or services. The public API overview is in [API.md](API.md); source contract and acceptance fixtures are authoritative for exact payloads. Compatibility exceptions are recorded in [native/contracts/compatibility-exceptions.json](../native/contracts/compatibility-exceptions.json).
