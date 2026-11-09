# Contributing to Gridstator

Thanks for your interest! Gridstator is early-stage; issues, design feedback and pull requests are
all welcome.

## Getting started

See [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) for setup, layout and tests. Before opening a pull
request:

```bash
pnpm check        # typecheck, lint, format check, unit tests
pnpm test:e2e     # if you touched the controller, agent, executors or CLI (needs Docker)
```

## Guidelines

- **Real behaviour only.** No mock data or placeholder endpoints in shipped code. Every UI action
  and CLI command must work end to end.
- **Keep the boundaries.** `protocol` defines contracts and `scheduler` stays pure (no I/O).
  Executors talk to Docker only through `DockerCli`. Dependencies point downward
  (see [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)).
- **State transitions** go through `Cluster.transition`/`finish` and the table in
  `packages/protocol/src/states.ts`. Add a test for any new transition or reconciliation rule.
- **Security-sensitive changes** (snapshot paths, auth, TLS, container options, cleanup) need a
  test that shows the protection working, and an update to [docs/SECURITY.md](docs/SECURITY.md).
- **Errors** should be `GridstatorError`s with a clear message and, where possible, a hint telling
  the user what to do next.
- **Docs** in `docs/` are kept in sync with the code. Update them in the same pull request.
- Strict TypeScript; avoid `any`. Format with Prettier.

## Reporting security issues

Please do not open public issues for vulnerabilities. Use GitHub's private security advisories.

## License

By contributing you agree that your contributions are licensed under the Apache License 2.0.
