# Project status and roadmap

Native v2 is the current development line. The [confirmed upgrade plan](V2_UPGRADE_PLAN.md) defines parity, release gates and the fresh-cluster cutover. [V2_PROGRESS.md](V2_PROGRESS.md) records actual implementation and validation evidence. The [native release ledger](../native/contracts/parity-ledger.json) remains authoritative for open acceptance rows.

| Track | Current state | Next public milestone |
| --- | --- | --- |
| Native CLI/controller/worker | Development preview; platform, service and signing gates remain open | Verified four-target v2 release |
| VS Code native extension | Thin VSIX candidate; not yet on Marketplace | `gridstator.gridstator` listing paired with the native release |
| Desktop companion | Separate unsigned preview | OS installer and lifecycle qualification after core v2 |
| Node v1 | Published v0.1.0 GitHub release | End support at stable v2 launch; deprecate any published npm package and retain the recovery archive |

The [release readiness guide](RELEASING.md) describes CI, security checks, artifact verification and owner actions. The [desktop plan](DESKTOP_COMPANION_PLAN.md) and [desktop progress](DESKTOP_COMPANION_PROGRESS.md) track the companion independently. The historical [v1 plan](legacy/v1/PLAN.md) remains available for the legacy release.
