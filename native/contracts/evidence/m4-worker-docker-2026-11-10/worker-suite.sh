#!/bin/sh
set -eu
base=/Users/philjepsen/gridstator-validation.I50ONpwm
export CARGO_HOME="$base/cargo"
export RUSTUP_HOME="$base/rustup"
export CARGO_TARGET_DIR="$base/source/native/target"
export PATH="$base/docker-tools/bin:/Applications/Docker.app/Contents/Resources/bin:$CARGO_HOME/bin:/usr/bin:/bin:/usr/sbin:/sbin"
export DOCKER_HOST=unix:///Users/philjepsen/.docker/run/docker.sock
export GRIDSTATOR_BUILD_ID=m4-memory-dev-181bd9221ed7
export GRIDSTATOR_SOURCE_REVISION=4b8f13ccf1b21820007fef0ab52143c961b69216
export GRIDSTATOR_CONFIG="$base/docker-suite-private/client.json"
export GRIDSTATOR_CONTROLLER_DATA="$base/docker-suite-private/controller"
export GRIDSTATOR_AGENT_DATA="$base/docker-suite-private/agent"
export GRIDSTATOR_INSTALL_ROOT="$base/docker-suite-private/install"
unset GRIDSTATOR_URL GRIDSTATOR_TOKEN GRIDSTATOR_CA_FILE GRIDSTATOR_SUPERVISED GRIDSTATOR_INSTALL_VERSION
cd "$base/source-memory-dev/native"
"$CARGO_HOME/bin/cargo" test --locked --target aarch64-apple-darwin --test worker_contract -- --ignored --skip independent_managed_worker_installs_restarts_rejoins_and_unmanaged_worker_stays_put --nocapture --test-threads=1
