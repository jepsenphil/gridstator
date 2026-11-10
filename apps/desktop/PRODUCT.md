# Gridstator desktop companion

<!-- impeccable:product-schema 1 -->

## Platform

web

The locally bundled worker window runs inside a native desktop shell on Windows, macOS and Linux.

## Users

The signed-in user controls workers owned by their account on this machine. On Windows, those workers run in configured WSL2 distributions.

## Product Purpose

Make local worker activity visible and let its owner pause admissions, inspect jobs and masked logs, set workload budgets and manage startup without opening VS Code.

## Capabilities and Constraints

The confirmed decisions and acceptance checks are in ../../docs/DESKTOP_COMPANION_PLAN.md. Local pause persists across reboots. Stop drains jobs; force-stop and cancellation require confirmation. Worker controls preserve the controller. Quit closes only the companion. Resource settings limit accepted workload, not total machine usage. Desktop setup offers Join a controller and Create a controller here. Headless installs stay headless. Tokens are never persisted in webview storage.

## Brand Commitments

Extend the existing Gridstator identity in ../vscode/webview/styles.css and ../vscode/media/gridstator.svg: compact system typography, four-square mark, restrained blue actions, explicit textual states and native utility controls.

## Evidence on Hand

The user confirmed the purpose, actions, states and onboarding through seventeen questions, then authorized building the plan. Worker snapshots and local masked logs provide production content. UI fixture data is confined to automated tests.

## Accessibility & Inclusion

Keyboard navigation, screen-reader labels, light/dark themes, display scaling and a usable window when Linux has no tray are part of the confirmed plan.
