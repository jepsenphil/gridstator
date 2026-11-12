# Security policy

Gridstator runs developer code on worker machines and stores source snapshots and job results on a controller. Report vulnerabilities **privately** through [GitHub security advisories](https://github.com/jepsenphil/gridstator/security/advisories/new), not public issues. Include the affected build, setup and a minimal reproduction. The project aims to acknowledge reports within one week.

## Supported versions

Gridstator 2.0.0 is a development preview and is planned as the first public native release. Security fixes are made on the current development line.

## Scope

Gridstator is designed for a trusted personal or small-team network. It is not a multi-tenant sandbox or an internet-facing service. The [native security guide](docs/SECURITY.md) documents credentials, trust boundaries, source and job-secret handling, CI scans and limits.
