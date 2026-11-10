set -eu
for tool in node npm rustc cargo zig; do
  if command -v "$tool" >/dev/null 2>&1; then echo "Unexpected runtime tool: $tool" >&2; exit 1; fi
done
echo 'No Node/npm/Rust/Cargo/Zig executable in runtime PATH'
umask 077
mkdir -p /tmp/runtime-proof
for kind in absolute relative missing-nested; do
  base=/tmp/config-mode-contract/$kind
  mkdir -p "$base/shared" "$base/home" "$base/xdg-config" "$base/xdg-data" "$base/xdg-cache" "$base/xdg-run" "$base/scratch" "$base/cache"
  chmod 775 "$base/shared"
  [ "$(stat -c %a "$base/shared")" = 775 ]
  case "$kind" in
    absolute) config="$base/shared/client.json"; file="$config" ;;
    relative) config=client.json; file="$base/shared/client.json" ;;
    missing-nested) config="$base/new/nested/client.json"; file="$config" ;;
  esac
  (
    cd "$base/shared"
    env HOME="$base/home" XDG_CONFIG_HOME="$base/xdg-config" XDG_DATA_HOME="$base/xdg-data" XDG_CACHE_HOME="$base/xdg-cache" XDG_RUNTIME_DIR="$base/xdg-run" TMPDIR="$base/scratch" TMP="$base/scratch" TEMP="$base/scratch" GRIDSTATOR_CONFIG="$config" GRIDSTATOR_CONTROLLER_DATA="$base/controller" GRIDSTATOR_AGENT_DATA="$base/agent" GRIDSTATOR_CACHE="$base/cache" /proof/gridstator controller init --host 127.0.0.1 --port 0 --data-dir "$base/controller" > "$base/private-init.log" 2>&1
  )
  [ "$(stat -c %a "$base/shared")" = 775 ]
  [ "$(stat -c %a "$file")" = 600 ]
  [ "$(stat -c %a "$base/controller")" = 700 ]
  if [ "$kind" = missing-nested ]; then
    [ "$(stat -c %a "$base/new")" = 700 ]
    [ "$(stat -c %a "$base/new/nested")" = 700 ]
  fi
  echo "CONFIG_PARENT_MODE_PASS case=$kind existing=775 file=600 dedicated=700"
done
echo 'CONFIG_PARENT_MODE_PASS new_nested=700,700'
/proof/gridstator build-info --json
/proof/gridstator --version
/proof/gridstator controller init --host 127.0.0.1 --port 0 --data-dir /tmp/controller
/proof/gridstator controller start --data-dir /tmp/controller > /tmp/controller.log 2>&1 &
controller_pid=$!
trap 'kill -TERM "$controller_pid" 2>/dev/null || true; wait "$controller_pid" || true' EXIT
attempt=0
until [ -f /tmp/controller/runtime.json ]; do
  kill -0 "$controller_pid"
  attempt=$((attempt + 1))
  [ "$attempt" -lt 100 ]
  sleep 0.1
done
port=$(sed -n 's/^[[:space:]]*"port": \([0-9]*\).*/\1/p' /tmp/controller/runtime.json)
[ -n "$port" ]
sed "s|https://127.0.0.1:0|https://127.0.0.1:$port|" /tmp/runtime-proof/client.json > /tmp/runtime-proof/client.next
mv /tmp/runtime-proof/client.next /tmp/runtime-proof/client.json
/proof/gridstator status --json
kill -TERM "$controller_pid"
wait "$controller_pid"
trap - EXIT
echo 'Clean Alpine live TLS controller status and graceful stop passed'
