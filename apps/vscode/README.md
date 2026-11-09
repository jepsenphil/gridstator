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

1. Set up a controller and workers (see the [project README](https://github.com/jepsenphil/gridstator#quick-start)).
2. Open a folder. The extension automatically reuses the CLI login for that remote account.
   On the controller host, the account that owns the controller data gets one shared admin login
   automatically. All open SSH windows observe login changes. On other machines, use
   **Gridstator: Sign In to Controller** or `gridstator login …` once.
3. Open the Gridstator view and press ▶ next to a task, workflow, script or Dockerfile.

Jobs run the files saved on disk; the extension asks before running when editors have unsaved
changes (`gridstator.unsavedFiles`).

Open **Settings** in the dashboard for connection details, editor preferences, component versions,
tokens, retention and worker update policies. Admins can find official releases or upload a
controller package; downloads are checksum verified and managed installs retain rollback support.
Worker updates wait for running jobs to finish by default. Controller updates wait for any worker
on the controller machine to finish before restarting.

Controller-host SSH windows can start, stop and restart the user service and apply listening
address/port changes. A public HTTPS URL lets workers learn the new endpoint. Previous listeners
remain available for offline or older workers; a failed migration restores the previous setup.
Overlapping bindings, such as changing `0.0.0.0` to a specific address on the same port, require a
new port so the old listener can remain available. These controls manage Linux/systemd user
services. Opening a window never starts a deliberately stopped controller.

Use **Restore Local Access** after revoking a local login. Custom configuration paths and explicit
environment connections remain authoritative. Disable `gridstator.autoConnect` to connect manually.
