# Gridstator Security

Gridstator runs developer code on other machines. This document states what it protects against,
what it explicitly does not, and the controls that implement those guarantees.

**Intended deployment:** one person or a small trusted team on a private LAN. Gridstator is not a
multi-tenant CI service and must not be exposed to the internet.

## 1. Trust model

| Party | Trust |
|---|---|
| Operators (holders of client tokens) | Trusted. Anyone who can submit jobs can run arbitrary code on every worker that accepts their jobs, and can read any snapshot. |
| Admins (client tokens with role `admin`) | Trusted; can additionally mint and revoke tokens. |
| Agents | Trusted to execute jobs and report honestly. A compromised worker can read the source of jobs sent to it and forge results for them. |
| Controller host | Fully trusted; holds the CA key, all token hashes, all snapshots, logs and job secrets. |
| Code inside jobs | **Semi-trusted.** It is your own project and its dependencies. Containers limit accidents, not determined attackers (see §3). |
| Other devices on the LAN | Untrusted. They must not be able to run jobs, read source, or impersonate the controller. |

## 2. Threats and controls

### 2.1 Network attacker on the LAN

- **All traffic is TLS**, including loopback. `controller init` generates an ECDSA P-256 cluster CA
  (10-year validity) and a server certificate signed by it.
- **Pinning:** login and join commands include the CA's SHA-256 fingerprint. The client downloads
  `/v1/pki/ca.pem`, refuses to continue unless the fingerprint matches, stores the CA, and from then
  on trusts **only** that CA (Node's default public roots are not used). An attacker without the CA
  key cannot impersonate the controller. Hostname checking is disabled because the pinned private CA
  is the trust anchor and LAN IP addresses change.
- **No unauthenticated execution endpoints.** Only `GET /v1/health` (version only) and the public CA
  certificate are served without a token.
- **Agents never listen** on the network. Workers only make outbound connections.
- **Docker's TCP API is never used or exposed.** Agents use the local `docker` CLI and socket.

### 2.2 Credential theft and misuse

- Tokens are 256-bit random values with type prefixes (`gsc_` client, `gsj_` join, `gsa_` agent).
  The controller stores only SHA-256 hashes, compared in constant time.
- Join tokens expire (default 24 h) and are single-use by default.
- Each agent has its own credential; removing a node revokes it. Client tokens are named and can be
  revoked individually; every job records the name of the token that submitted it.
- Credentials live in files with mode 0600 (`client.json`, `agent.json`, the controller's `pki/`
  and database); data directories are created with mode 0700.
- Tokens are never accepted in URLs or query strings, and are never logged. Fastify request logging
  redacts the `authorization` header.

### 2.3 Source and secret leakage

- **Snapshot excludes:** `.git/`, `node_modules/`, everything git ignores, and secret patterns even
  when tracked: `.env`, `.env.*` (except `.env.example`, `.env.sample`, `.env.template`,
  `.env.defaults`), `*.pem`, `*.key`, `*.p12`, `*.pfx`, `*.jks`, `*.keystore`, `id_rsa*`,
  `id_ecdsa*`, `id_ed25519*`, `.netrc`, `.secrets`, `.pypirc`, `.ssh/`, `.aws/`, `.docker/config.json`, and
  `.npmrc` files that contain auth tokens. Each exclusion is reported as a warning. A project can
  re-admit specific files with `snapshot.allowSecrets` in `gridstator.yml`.
- **Secrets in jobs:** values passed with `--secret-env NAME` are read from the client environment,
  never placed on any command line (the agent passes environment through a 0600 `--env-file` that is
  deleted right after `docker create`), masked as `***` in streamed logs, and redacted from every API
  response. They are stored in the controller database so that retries reproduce the original job;
  protect the controller host accordingly.
- **VS Code job secrets:** trusted workspaces resolve configured `secretEnv` names from the
  extension host environment or host/account/project-scoped VS Code SecretStorage. Missing values are
  collected through native password prompts and stored encrypted, never in workspace settings,
  webview messages/state, snapshot files or process environment. Cancellation/storage failure
  stops submission. Configured secret names in plain Run/task environment fields are rejected to
  prevent an unredacted duplicate. The **Clear Saved Job Secrets** command removes selected saved
  values. VS Code controls platform storage and encryption; these secrets do not sync to other
  machines. Once submitted, the existing controller retention and redaction rules above
  still apply.
- Snapshots and logs are retained only as long as configured (see ARCHITECTURE §7).

### 2.4 Malicious or malformed snapshot content

- Manifest paths are validated on the controller and the agent: relative, normalised, no `..`
  segments, no absolute or drive-letter paths, no NUL or backslashes, no duplicates, and no path
  nested under a file or symlink entry.
- Symlinks whose target is absolute or resolves outside the snapshot root are excluded at capture
  and rejected at materialization.
- Every blob is verified against its SHA-256 digest on upload (controller) and on download (agent).
- The default staging path never writes snapshot content to the worker's host filesystem: the agent
  builds a tar from validated entries and streams it into a per-job Docker volume. The host-directory
  extractor (used to stage builds and workflows) creates a fresh directory, refuses to follow or overwrite
  existing entries, and writes with `O_EXCL`.

### 2.5 Interference with the worker host

- Gridstator only stops, removes or garbage-collects Docker objects labelled
  `io.gridstator.managed=true` **and** `io.gridstator.node=<this node>`. Unrelated containers are
  never touched.
- Containers run without `--privileged`, with `--security-opt no-new-privileges`, a PID limit, CPU
  and memory limits, and `--init` so the whole process tree is signalled on stop.
- Mounts are fixed: the job's workspace volume and the shared package-cache volume. Host paths and
  the Docker socket are **not** mountable through the job API.
- Per-node CPU/RAM reservations and live available-memory checks keep jobs from starving interactive
  work on development machines.

### 2.6 Docker builds

- Build secrets (`--secret-env`) reach BuildKit as `--secret id=NAME,env=NAME`: the value is only in
  the buildx process environment and is available to `RUN --mount=type=secret` steps, never written
  to an image layer, the command line or the log (it is masked like any secret).
- Build arguments are **not** secret: they can end up in image history. Use secrets for credentials.
- Dockerfile and context paths must resolve inside the staged snapshot (symlinks are followed and
  checked); builds labelled with the job ID; the staging directory is removed after the build.
- Builds run inside the worker's Docker daemon, so per-job CPU/memory limits are a scheduling
  reservation, not an enforced cgroup limit.

### 2.7 GitHub Actions workflows (act)

Workflows are code that runs on your workers, including third-party actions that act downloads from
GitHub. Run only workflows you would run on your own machine; Gridstator never triggers workflows
from external events (pushes, pull requests). Defaults chosen to limit the blast radius:

- The worker's Docker socket is **not** mounted into workflow containers unless the job opts in
  (`dockerSocket` / `--docker-socket`), and act's container-options policy then restricts
  workflow-supplied container options and host-path volumes. With the socket mounted, a workflow is
  root-equivalent on the worker.
- Secrets, vars and environment are written to private 0600 files for act and deleted afterwards;
  act never reads `.secrets`, `.vars` or `.env` from the project (and `.secrets` is excluded from
  snapshots like `.env`). act masks secret values in output and Gridstator masks them again.
- act's cache server is bound to `127.0.0.1` (act binds it to the LAN address by default) and its
  artifact server listens on loopback on a per-job port.
- act's containers are labelled with the job and node and removed by Gridstator if act leaves any
  behind.
- `GITHUB_TOKEN` is only available if you forward one explicitly (`--secret-env GITHUB_TOKEN`).

### 2.8 VS Code extension

- The dashboard webview runs with a strict Content Security Policy (scripts only with a per-load
  nonce, local resources only) and has no network access to the controller: every action is a typed
  message handled by the extension.
- The extension uses the same client configuration file as the CLI (mode 0600) and the same CA
  pinning; sign-in verifies the CA fingerprint before saving anything.
- Secrets named in `gridstator.yml` are read from the environment of the VS Code (or Remote SSH)
  server process; they are not stored by the extension.

## 3. What Gridstator does not protect against

- **Containers are not a sandbox for hostile code.** Docker shares the host kernel and the Docker
  daemon is root-equivalent. Do not run untrusted code (for example, workflows from public pull
  requests) on your machines. Gridstator never triggers execution automatically from external events.
- **Operators are all-powerful by design.** There is no per-project authorization in the initial
  release.
- **A compromised controller host compromises the cluster** (it can send any job to any worker).
- **Denial of service on the LAN** is out of scope.
- **Network egress from jobs is not restricted**; jobs can reach your LAN like any container.

## 4. Operational guidance

- Keep the controller port reachable from your LAN only. Do not forward it through your router.
- Rotate a token by creating a new one and revoking the old one (`gridstator token revoke`).
- If a machine is lost, `gridstator nodes remove <node>` revokes its credential immediately.
- To rotate the CA, re-run `gridstator controller init --force` and re-pair clients and agents.
- Use `gridstator doctor` to check pinning, permissions and token validity.

## 5. Reporting vulnerabilities

Please report vulnerabilities privately through GitHub Security Advisories on the project
repository rather than in public issues.
