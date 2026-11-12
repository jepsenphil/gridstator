# Security policy

Gridstator runs developer code on worker machines and stores source snapshots and job results on a controller. Report vulnerabilities **privately** through [GitHub security advisories](https://github.com/jepsenphil/gridstator/security/advisories/new), not public issues. Include the affected build, setup and a minimal reproduction. The project aims to acknowledge reports within one week.

## Supported versions

| Line | Status |
| --- | --- |
| Native v2 | Development preview; no public stable release yet |
| Node v0.1.x | Current published release; deprecated in favor of native v2 and supported until stable v2 launches |

At stable v2 launch, v1 support ends. Its [tagged release](https://github.com/jepsenphil/gridstator/releases/tag/v0.1.0) and [recovery guides](docs/legacy/v1/README.md) remain available.

## Scope

Gridstator is designed for a trusted personal or small-team network. It is not a multi-tenant sandbox or an internet-facing service. The [native security guide](docs/SECURITY.md) documents credentials, trust boundaries, source and job-secret handling, CI scans and limits. The [v1 guide](docs/legacy/v1/SECURITY.md) covers the older implementation.
