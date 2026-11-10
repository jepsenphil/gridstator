# Desktop offline native setup verification

The Windows x64 NSIS installer was rebuilt with real Linux x64 and ARM64 CLI archives from native
build `native-desktop-20261110-d99c20f05013` and its complete four-target release manifest. macOS
payloads were built on the existing Apple Silicon Mac, with Intel build-info executed under
Rosetta. Linux x64 was executed locally; Linux ARM64 was cross-compiled and not executed here.

`packaged-install.json` records a successful install using resources extracted from the final
Windows setup executable, with `curl` replaced by a command that always fails. It checks the real
CLI's required capabilities, the retained service manifest and the second architecture's archive
digest. The temporary extraction and installation directories were removed after verification.

`native-bootstrap.json` records successful preparation of a verified managed service payload store
from the installed CLI. The pre-existing host service definition was preserved and no service
manager command ran. Its temporary install/store directories were removed afterward.

`installer-integrity.log` records the final NSIS archive integrity check. The embedded desktop
executable's GUI subsystem and compiled manifest/archive digest pins were also checked directly.

Regression checks passed: seven Node packaging tests, twelve portable Rust backend tests, four
native shell installer tests, twelve browser tests, desktop TypeScript, scoped ESLint, formatting
and Linux/Windows portable-backend Clippy. The native regression rejects an archive and matching
checksum file rewritten together, before that archive's helper can execute.

This is an unsigned development preview. Windows application execution, WSL path translation on
an actual Windows host, installer upgrades and sign-in/lifecycle qualification were not run here.
No release was published and no production service or startup preference was changed.

`host-isolation-dev2.json` and `installer-dev2-integrity.log` record the subsequent
`2.0.0-dev.2` installer. A Windows process inventory confirmed the installed GUI was using an older
Downloads preview's background host; the user confirmed setup proceeded after stopping that legacy
host. The new routing regression failed with account-only endpoints and passes when endpoints also
identify the installation path and executable bytes. Thirteen portable backend tests pass; the
additional two-endpoint Windows transport regression is compiled but was not executed here.
