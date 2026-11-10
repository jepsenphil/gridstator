# Gridstator for VS Code

**Multiple machines. One workflow.** Run builds, tests and GitHub Actions workflows on the
computers in your network, from VS Code, against the exact files in your workspace (including
unsaved-to-git changes). Works in local folders and over Remote SSH: the extension runs where your
code lives.

## Features

- **Gridstator Activity Bar** with your cluster's nodes, running and recent jobs, and everything
  runnable in the workspace: `gridstator.yml` tasks and builds, GitHub Actions workflows and jobs,
  package.json scripts, and Dockerfiles (with their stages).
- **Dashboard**: cluster overview, job list with filters, job details with live searchable logs,
  steps, image details and artifact downloads, a launcher with worker selection and options, and
  node management (enable, drain, disable, concurrency, reserved resources, labels).
- **Cancel, retry and rerun** jobs from the dashboard, the Jobs view or notifications.
- **Problems panel**: compiler, linter and test failures from remote logs, mapped to your files.
- **Testing view**: JUnit reports from job artifacts, and a *Run on Gridstator* test profile.
- **Status bar**: nodes online, running and queued jobs.

## Getting started

1. Open a folder and the Gridstator view. Choose **Set up a controller here** or **Connect to an
   existing controller**. In SSH and WSL windows, “here” means the workspace host.
2. Local setup installs a verified native CLI for your account if needed, creates a controller and
   worker as a background user service, and signs you in automatically. The service survives
   closing VS Code and starts at OS sign-in. The worker waits for Docker if it is unavailable.
   Existing CLI logins and controllers owned by this account are reused automatically; all windows
   share login changes. To connect elsewhere, paste the complete `gridstator login …` command or
   enter the URL, fingerprint and token manually.
3. Open the Gridstator view and press ▶ next to a task, workflow, script or Dockerfile.

Jobs run the files saved on disk; the extension asks before running when editors have unsaved
changes (`gridstator.unsavedFiles`).

To add another worker, open **Nodes → Add worker**, use the **+** button in the Cluster view, or
run **Gridstator: Generate Worker Join Command** from the command palette. With an admin login,
name the command, check the controller's LAN address, then generate and copy it. It expires after
24 hours and allows one worker to join. The command is shown only in memory; revoke access in
**Settings → Access tokens** if needed.

Paste the command into **Join a controller → Worker join command** in the Gridstator desktop
app on the other computer. On Windows, choose its WSL2 distribution; the app starts the worker.
For terminal setup on Linux, macOS or inside WSL, run the copied command with the native CLI,
then `gridstator agent install-service`. Docker must be available on workers to run jobs.

Open **Settings** in the dashboard for connection details, editor preferences, component versions,
tokens, retention and worker update policies. Admins can find official releases or upload a
controller package; downloads are checksum verified and managed installs retain rollback support.
Worker updates wait for running jobs to finish by default. Controller updates wait for any worker
on the controller machine to finish before restarting.

To rename a worker, edit **Nodes → Worker name** and save, or right-click it in the Cluster tree
and select **Rename Worker**. This changes its Gridstator name, leaving the machine's hostname,
worker identity, credentials, settings and job history intact. Existing queued jobs and retries
remain bound to that worker. New task configurations should use the new name or its stable node ID.

Controller-host SSH windows can start, stop and restart the user service and apply listening
address/port changes. A public HTTPS URL lets workers learn the new endpoint. Previous listeners
remain available for offline or older workers; a failed migration restores the previous setup.
Overlapping bindings, such as changing `0.0.0.0` to a specific address on the same port, require a
new port so the old listener can remain available. These controls manage Linux/systemd user
services. Opening a window never starts a deliberately stopped controller.

Use **Restore Local Access** after revoking a local login. Custom configuration paths and explicit
environment connections remain authoritative. Disable `gridstator.autoConnect` to connect manually.

Interrupted local setup offers **Finish controller setup** and preserves the controller identity
and shared login. Existing native executables are never replaced by first-run setup; update an
older CLI manually if it lacks controller setup support. Automatic installation requires a
published native release with `local-setup-v1` support. Manual CLI setup remains available in the
[project README](https://github.com/jepsenphil/gridstator#quick-start).

On Linux, setup also tries to keep the user service running after the last SSH session closes.
If the host denies this, setup and Settings explain how to enable user-service lingering; the
controller remains usable in the current session.
