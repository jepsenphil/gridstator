# Changelog

All notable changes are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
and the project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Native architecture, installation, security, release and screenshot guides.
- Repository Run tasks and CI checks for native builds, VS Code packaging and dependency security audits.
- Manual release preparation for the `gridstator.gridstator` Marketplace extension with trusted publishing.

### Changed

- Removed the unused Node CLI, controller, worker and release packaging. Every VSIX now delegates to the native executable.
- Replaced Node parity checks with native behavior tests and five platform release acceptance gates.
- Updated native XML and ZIP dependencies to clear high and critical RustSec advisories.
