set -eu
export CARGO_HOME=/Users/philjepsen/gridstator-validation.I50ONpwm/cargo
export RUSTUP_HOME=/Users/philjepsen/gridstator-validation.I50ONpwm/rustup
export CARGO_TARGET_DIR=/Users/philjepsen/gridstator-validation.I50ONpwm/source/native/target
export PATH="$CARGO_HOME/bin:/usr/bin:/bin:/usr/sbin:/sbin"
export GRIDSTATOR_BUILD_ID=m4-memory-dev-181bd9221ed7
export GRIDSTATOR_SOURCE_REVISION=4b8f13ccf1b21820007fef0ab52143c961b69216
mkdir -p /Users/philjepsen/gridstator-validation.I50ONpwm/memory-dev-evidence
cd /Users/philjepsen/gridstator-validation.I50ONpwm/source-memory-dev/native
cargo test --locked --target aarch64-apple-darwin --lib host_available_memory -- --nocapture > /Users/philjepsen/gridstator-validation.I50ONpwm/memory-dev-evidence/memory-tests-arm64.log 2>&1
cargo clippy --locked --all-targets --target aarch64-apple-darwin -- -D warnings > /Users/philjepsen/gridstator-validation.I50ONpwm/memory-dev-evidence/clippy-arm64.log 2>&1
cargo test --locked --target x86_64-apple-darwin --lib host_available_memory -- --nocapture > /Users/philjepsen/gridstator-validation.I50ONpwm/memory-dev-evidence/memory-tests-rosetta.log 2>&1
/usr/bin/nice -n 10 cargo build --release --locked --target aarch64-apple-darwin -j2 > /Users/philjepsen/gridstator-validation.I50ONpwm/memory-dev-evidence/release-build-arm64.log 2>&1
cp "$CARGO_TARGET_DIR/aarch64-apple-darwin/release/gridstator" /Users/philjepsen/gridstator-validation.I50ONpwm/payloads/m4-memory-dev-181bd9221ed7-arm64
chmod 555 /Users/philjepsen/gridstator-validation.I50ONpwm/payloads/m4-memory-dev-181bd9221ed7-arm64
/Users/philjepsen/gridstator-validation.I50ONpwm/payloads/m4-memory-dev-181bd9221ed7-arm64 build-info --json > /Users/philjepsen/gridstator-validation.I50ONpwm/memory-dev-evidence/build-info.json
echo 'temporary dev memory tests, strict checks, ARM release built'
