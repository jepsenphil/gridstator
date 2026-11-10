#!/bin/sh
set -eu
base=/Users/philjepsen/gridstator-validation.I50ONpwm
export PATH=/usr/bin:/bin:/usr/sbin:/sbin
docker() { /Applications/Docker.app/Contents/Resources/bin/docker "$@"; }
export DOCKER_HOST=unix:///Users/philjepsen/.docker/run/docker.sock
export DOCKER_CONFIG="$base/docker-tools/anonymous-docker-config"
mkdir -p "$DOCKER_CONFIG"
chmod 700 "$DOCKER_CONFIG"
if ! test -f "$DOCKER_CONFIG/config.json"; then
    printf '%s\n' '{"cliPluginsExtraDirs":["/Applications/Docker.app/Contents/Resources/cli-plugins"]}' > "$DOCKER_CONFIG/config.json"
    chmod 600 "$DOCKER_CONFIG/config.json"
fi
expected_build=$1
expected_sha=$2
binary="$base/docker-tools/$expected_build-linux-arm64"
printf '%s\n' "$expected_sha  $binary" | shasum -a 256 -c -
docker pull --platform linux/arm64 alpine:3.21
docker image inspect alpine:3.21 --format '{{json .}}' > "$base/docker-tools/$expected_build-linux-arm-runtime-image.json"
docker run --rm --name "gridstator-arm-runtime-${expected_build#native-preview-}" --label "io.gridstator.validation=$expected_build-linux-arm-runtime" --platform linux/arm64 --network none --read-only --cap-drop ALL --security-opt no-new-privileges --cpus 1 --memory 256m --user 65534:65534 --tmpfs /tmp:rw,nosuid,nodev,size=64m -e "EXPECTED_BUILD_ID=$expected_build" -e HOME=/tmp -e GRIDSTATOR_CONFIG=/tmp/client.json -e GRIDSTATOR_CONTROLLER_DATA=/tmp/controller --mount "type=bind,source=$binary,target=/gridstator,readonly" -i alpine:3.21 sh -s <<'INNER'
set -eu
test "$(uname -m)" = aarch64
uname -srm
for tool in node npm rustc cargo python python3 zig sqlite3 openssl; do
  if command -v "$tool" >/dev/null 2>&1; then echo "Unexpected runtime tool: $tool"; exit 1; fi
done
echo 'No Node/npm/Rust/Python/Zig/SQLite/OpenSSL executables in runtime PATH'
/gridstator build-info --json
/gridstator --version
tmp_mode=$(stat -c '%a' /tmp)
/gridstator controller init --host 127.0.0.1 --port 17499 >/tmp/init.out 2>/tmp/init.err
test "$(stat -c '%a' /tmp)" = "$tmp_mode"
test "$(stat -c '%a' /tmp/client.json)" = 600
test "$(stat -c '%a' /tmp/controller)" = 700
echo 'Unprivileged custom config in disposable shared tmpfs: parent unchanged, file0600, data0700'
/gridstator controller start >/tmp/controller.out 2>/tmp/controller.err &
controller_pid=$!
trap 'kill -TERM "$controller_pid" 2>/dev/null || true; wait "$controller_pid" 2>/dev/null || true' EXIT
attempt=0
until /gridstator status --json >/tmp/status.json 2>/tmp/status.err; do
  attempt=$((attempt+1))
  if test "$attempt" -ge 100; then cat /tmp/status.err /tmp/controller.err; exit 1; fi
  sleep .1
done
cat /tmp/status.json
grep -q 'linux/arm64' /tmp/status.json
grep -q "$EXPECTED_BUILD_ID" /tmp/status.json
kill -TERM "$controller_pid"
wait "$controller_pid"
trap - EXIT
test ! -e /tmp/controller/runtime.json
echo 'Actual Linux ARM64 clean offline Alpine TLS lifecycle/graceful cleanup passed'
INNER
