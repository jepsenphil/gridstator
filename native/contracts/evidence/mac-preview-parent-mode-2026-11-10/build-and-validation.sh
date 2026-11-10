set -eu
export CARGO_HOME=/Users/philjepsen/gridstator-validation.I50ONpwm/cargo
export RUSTUP_HOME=/Users/philjepsen/gridstator-validation.I50ONpwm/rustup
export CARGO_TARGET_DIR=/Users/philjepsen/gridstator-validation.I50ONpwm/source/native/target
export PATH="$CARGO_HOME/bin:/usr/bin:/bin:/usr/sbin:/sbin"
export GRIDSTATOR_BUILD_ID=native-preview-20261110-6994d2cc804b
export GRIDSTATOR_SOURCE_REVISION=4b8f13ccf1b21820007fef0ab52143c961b69216
export GRIDSTATOR_MAC_VALIDATION_ROOT=/Users/philjepsen/gridstator-validation.I50ONpwm/preview-roles-6994d2cc804b
mkdir -p /Users/philjepsen/gridstator-validation.I50ONpwm/preview-evidence-6994d2cc804b "$GRIDSTATOR_MAC_VALIDATION_ROOT"
chmod 700 /Users/philjepsen/gridstator-validation.I50ONpwm/preview-evidence-6994d2cc804b "$GRIDSTATOR_MAC_VALIDATION_ROOT"
cd /Users/philjepsen/gridstator-validation.I50ONpwm/source-preview-6994d2cc804b
/usr/bin/python3 - <<'VERIFY'
import pathlib,json,hashlib
meta=json.loads(pathlib.Path('PREVIEW_CHECKPOINT.json').read_text())
for entry in meta['files']:
 assert hashlib.sha256(pathlib.Path(entry['path']).read_bytes()).hexdigest()==entry['sha256'],entry['path']
print('Every production/build input matches common preview checkpoint')
VERIFY
cd native
for target in aarch64-apple-darwin x86_64-apple-darwin; do
 case "$target" in aarch64*) short=arm64;; *) short=x86_64;; esac
 echo "stage:state-tests-$short"
 cargo test --locked --target "$target" --lib state::tests -- --nocapture > /Users/philjepsen/gridstator-validation.I50ONpwm/preview-evidence-6994d2cc804b/state-tests-$short.log 2>&1
 cargo test --locked --target "$target" --test cli_contract custom_absolute_and_relative_config_paths_preserve_parent_permissions -- --nocapture > /Users/philjepsen/gridstator-validation.I50ONpwm/preview-evidence-6994d2cc804b/cli-parent-tests-$short.log 2>&1
 echo "stage:memory-tests-$short"
 cargo test --locked --target "$target" --lib host_available_memory -- --nocapture > /Users/philjepsen/gridstator-validation.I50ONpwm/preview-evidence-6994d2cc804b/memory-tests-$short.log 2>&1
 echo "stage:buildkit-tests-$short"
 cargo test --locked --target "$target" --lib executor::build::tests -- --nocapture > /Users/philjepsen/gridstator-validation.I50ONpwm/preview-evidence-6994d2cc804b/buildkit-tests-$short.log 2>&1
 echo "stage:strict-clippy-$short"
 cargo clippy --locked --all-targets --target "$target" -- -D warnings > /Users/philjepsen/gridstator-validation.I50ONpwm/preview-evidence-6994d2cc804b/clippy-$short.log 2>&1
 echo "stage:release-build-$short"
 /usr/bin/nice -n 10 cargo build --release --locked --target "$target" -j 2 > /Users/philjepsen/gridstator-validation.I50ONpwm/preview-evidence-6994d2cc804b/release-build-$short.log 2>&1
 cp "$CARGO_TARGET_DIR/$target/release/gridstator" /Users/philjepsen/gridstator-validation.I50ONpwm/payloads/native-preview-20261110-6994d2cc804b-$short
 chmod 555 /Users/philjepsen/gridstator-validation.I50ONpwm/payloads/native-preview-20261110-6994d2cc804b-$short
 export GRIDSTATOR_TEST_BINARY=/Users/philjepsen/gridstator-validation.I50ONpwm/payloads/native-preview-20261110-6994d2cc804b-$short
 "$GRIDSTATOR_TEST_BINARY" build-info --json > /Users/philjepsen/gridstator-validation.I50ONpwm/preview-evidence-6994d2cc804b/build-info-$short.json
 /usr/bin/python3 /Users/philjepsen/gridstator-validation.I50ONpwm/parent-modes-final.py "$GRIDSTATOR_TEST_BINARY" "$GRIDSTATOR_BUILD_ID" /Users/philjepsen/gridstator-validation.I50ONpwm/preview-evidence-6994d2cc804b/release-parent-modes-$short.json
 echo "stage:release-roles-$short"
 cargo test --locked --target "$target" --test launchd_e2e -- --ignored --nocapture > /Users/philjepsen/gridstator-validation.I50ONpwm/preview-evidence-6994d2cc804b/release-roles-$short.log 2>&1
 unset GRIDSTATOR_TEST_BINARY
 echo "stage:done-$short"
done
