# M4 Docker Desktop preparation

The user selected Docker Desktop for actual Mac worker validation. Version 4.99.0
was downloaded from Docker's official ARM64 URL, checked against its published
SHA-256, mounted read-only, and accepted by macOS Gatekeeper as notarized Developer ID
software. Deep strict signature verification passed before and after copying to
`/Applications/Docker.app`; no existing application was overwritten.

The application has been opened. Agreement/setup is left to the user; a subsequent
actual Docker engine probe returned version 29.9.3 with exit 0. Worker, capacity and
restart acceptance remain pending. Commands and bounded
verification are recorded in [evidence.json](evidence.json) and [installation.log](installation.log).
