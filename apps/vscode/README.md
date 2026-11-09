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
2. Run **Gridstator: Sign In to Controller** (or `gridstator login …` in a terminal on the same machine).
3. Open the Gridstator view and press ▶ next to a task, workflow, script or Dockerfile.

Jobs run the files saved on disk; the extension asks before running when editors have unsaved
changes (`gridstator.unsavedFiles`).
