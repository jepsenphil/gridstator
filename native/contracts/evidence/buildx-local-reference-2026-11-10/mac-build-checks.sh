set -eu
base=/Users/philjepsen/gridstator-validation.I50ONpwm
export CARGO_HOME="$base/cargo"
export RUSTUP_HOME="$base/rustup"
export CARGO_TARGET_DIR="$base/source/native/target"
export PATH="$base/docker-tools/bin:/Applications/Docker.app/Contents/Resources/bin:$CARGO_HOME/bin:/usr/bin:/bin:/usr/sbin:/sbin"
export DOCKER_HOST=unix:///Users/philjepsen/.docker/run/docker.sock
export GRIDSTATOR_DOCKER=/Applications/Docker.app/Contents/Resources/bin/docker
export GRIDSTATOR_ACT="$base/docker-tools/bin/act"
export GRIDSTATOR_BUILD_ID=buildx-fallback-dev-8e696aa15b2e
export GRIDSTATOR_SOURCE_REVISION=4b8f13ccf1b21820007fef0ab52143c961b69216
mkdir -p "$base/buildx-fallback-evidence/docker-config"
chmod 700 "$base/buildx-fallback-evidence" "$base/buildx-fallback-evidence/docker-config"
export DOCKER_CONFIG="$base/buildx-fallback-evidence/docker-config"
printf '%s\n' '{"auths":{}}' > "$DOCKER_CONFIG/config.json"
chmod 600 "$DOCKER_CONFIG/config.json"
mkdir -p "$DOCKER_CONFIG/cli-plugins"
ln -sf /Applications/Docker.app/Contents/Resources/cli-plugins/docker-buildx "$DOCKER_CONFIG/cli-plugins/docker-buildx"
cd "$base/source-buildx-fallback-dev/native"
docker version --format '{{.Client.Version}} / {{.Server.Version}}' > "$base/buildx-fallback-evidence/docker-version.txt"
docker buildx version > "$base/buildx-fallback-evidence/buildx-version.txt"
cargo test --locked --target aarch64-apple-darwin --lib executor::build::tests -- --nocapture > "$base/buildx-fallback-evidence/unit.log" 2>&1
cargo test --locked --target aarch64-apple-darwin --test executor_contract real_buildx_ -- --ignored --nocapture --test-threads=1 > "$base/buildx-fallback-evidence/real-buildx.log" 2>&1
cargo clippy --locked --all-targets --target aarch64-apple-darwin -- -D warnings > "$base/buildx-fallback-evidence/clippy.log" 2>&1
printf '%s\n' 'Mac fallback unit, actual Buildx and strict checks passed'
