# Changelog

All notable changes are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
and the project uses [Semantic Versioning](https://semver.org/).

## [0.1.0] - 2026-11-09

First public release.

### Added

- Controller and worker agents with a self-generated cluster CA, fingerprint pinning, and named,
  revocable client, join and agent tokens.
- Remote command execution in labelled, resource-limited Docker containers with live log streaming,
  cancel, retry, automatic retry of lost jobs, and persistent history with an audit trail.
- Content-addressed working-tree snapshots: uncommitted and untracked files included, `.gitignore`
  respected, secrets excluded, only changed files transferred.
- Capability, label and resource-aware scheduling with host reservations, concurrency limits,
  priorities, drain, preferred workers and warm-cache affinity.
- Docker image builds with BuildKit on workers (`gridstator build`): target stages, build args and
  secrets, tags, push, and image export as an artifact.
- GitHub Actions workflows with act against the working tree (`gridstator actions`), with
  hardened defaults, step progress and artifacts.
- Job groups and test sharding (`--shards N`, `{shard}`/`{total}` placeholders) with shards spread
  across workers and combined JUnit results.
- VS Code extension: Activity Bar views, dashboard (live logs, steps, artifacts, groups, node
  management), Problems panel and Testing view integration, Remote SSH support.
- `install-service` commands for systemd, release packaging (single-file npm package and VSIX).
