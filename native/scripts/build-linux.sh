#!/usr/bin/env bash
# Build-host tooling only. The resulting executables have no Node/Rust/Zig runtime dependency.
set -euo pipefail
selection=${1:-both}
case "$selection" in both|linux/amd64|linux/arm64) ;; *) echo 'usage: build-linux.sh [both|linux/amd64|linux/arm64]' >&2; exit 2 ;; esac
cd "$(dirname "$0")/.."
[[ $(uname -s) == Linux ]] || { echo 'Run this builder in the configured Linux Rust container' >&2; exit 2; }
case $(uname -m) in
  x86_64) zig_host=x86_64-linux; zig_sha=3581863e3cbcfd0fce0efc682ce7e9e93da96668a977791d356a26d2a7e9b17a ;;
  aarch64|arm64) zig_host=aarch64-linux; zig_sha=a1546d5646eb184437918d6635ca6b762a0e889eac09cef720ee7a1ed3fe7795 ;;
  *) echo 'Build container must run on x64 or ARM64 Linux' >&2; exit 2 ;;
esac
toolchain=$(sed -n 's/^channel = "\([^"]*\)"/\1/p' rust-toolchain.toml)
version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)
export GRIDSTATOR_BUILD_ID=${GRIDSTATOR_BUILD_ID:-native-selfbuild-$(cat /proc/sys/kernel/random/uuid)}
export GRIDSTATOR_SOURCE_REVISION=${GRIDSTATOR_SOURCE_REVISION:-unknown}
[[ "$GRIDSTATOR_BUILD_ID" =~ ^[a-zA-Z0-9_.-]{1,128}$ ]] || { echo 'Invalid build ID' >&2; exit 2; }
[[ "$GRIDSTATOR_SOURCE_REVISION" == unknown || "$GRIDSTATOR_SOURCE_REVISION" =~ ^[a-fA-F0-9]{40}$ ]] || { echo 'Source revision must be a full Git SHA or unknown' >&2; exit 2; }
export CARGO_HOME=${GRIDSTATOR_BUILD_CARGO_HOME:-/gridstator/cache/cargo}
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-/gridstator/cache/gridstator-self-build/target}
mkdir -p "$CARGO_HOME" "$CARGO_TARGET_DIR" self-build
build_tools=$(mktemp -d)
trap 'rm -rf "$build_tools"' EXIT
curl --fail --location --proto '=https' --tlsv1.2 --retry 3 "https://ziglang.org/download/0.17.1/zig-$zig_host-0.17.1.tar.xz" --output "$build_tools/zig.tar.xz"
printf '%s  %s\n' "$zig_sha" "$build_tools/zig.tar.xz" | sha256sum --check --status
tar -xJf "$build_tools/zig.tar.xz" -C "$build_tools"
export GRIDSTATOR_BUILD_ZIG="$build_tools/zig-$zig_host-0.17.1/zig"
cat > "$build_tools/cc" <<'WRAPPER'
#!/usr/bin/env bash
set -euo pipefail
filtered=()
for argument in "$@"; do
  case "$argument" in --target=*) ;; *) filtered+=("$argument") ;; esac
done
exec "$GRIDSTATOR_BUILD_ZIG" cc -target "$GRIDSTATOR_BUILD_ZIG_TARGET" "${filtered[@]}"
WRAPPER
chmod 755 "$build_tools/cc"
host=$(rustc +"$toolchain" -vV | sed -n 's/^host: //p')
linker="$(rustc +"$toolchain" --print sysroot)/lib/rustlib/$host/bin/rust-lld"
for key in linux/amd64 linux/arm64; do
  [[ "$selection" == both || "$selection" == "$key" ]] || continue
  case "$key" in
    linux/amd64) target=x86_64-unknown-linux-musl; export GRIDSTATOR_BUILD_ZIG_TARGET=x86_64-linux-musl ;;
    linux/arm64) target=aarch64-unknown-linux-musl; export GRIDSTATOR_BUILD_ZIG_TARGET=aarch64-linux-musl ;;
  esac
  rustup target add --toolchain "$toolchain" "$target"
  target_variable=${target//-/_}
  target_upper=${target_variable^^}
  env "CC_$target_variable=$build_tools/cc" "CFLAGS_$target_variable=-fno-sanitize=all" \
    "CARGO_TARGET_${target_upper}_LINKER=$linker" "CARGO_TARGET_${target_upper}_RUSTFLAGS=-C linker-flavor=ld.lld" \
    cargo +"$toolchain" build --locked --release --target "$target" --jobs "${GRIDSTATOR_BUILD_JOBS:-2}"
  destination="self-build/${key//\//-}"
  mkdir -p "$destination"
  cp "$CARGO_TARGET_DIR/$target/release/gridstator" "$destination/gridstator"
  readelf -l -d "$destination/gridstator" > "$destination/linkage.txt"
  if grep -Eq 'INTERP|NEEDED' "$destination/linkage.txt"; then echo 'Linux artifact must be static' >&2; exit 1; fi
  digest=$(sha256sum "$destination/gridstator" | cut -d ' ' -f 1)
  size=$(wc -c < "$destination/gridstator" | tr -d ' ')
  printf '{"version":"%s","buildId":"%s","sourceRevision":"%s","target":"%s","sha256":"%s","size":%s,"runtime":"rust","sourceState":"working-tree-snapshot","runtimeValidated":false}\n' \
    "$version" "$GRIDSTATOR_BUILD_ID" "$GRIDSTATOR_SOURCE_REVISION" "$key" "$digest" "$size" > "$destination/build-info.json"
  (cd "$destination" && sha256sum gridstator > SHA256SUMS)
  file "$destination/gridstator"
done
(find src scripts contracts -type f -print0; printf '%s\0' Cargo.toml Cargo.lock build.rs rust-toolchain.toml ../LICENSE ../scripts/install-native.sh) |
  sort -z | xargs -0 sha256sum > self-build/SOURCE_SHA256SUMS
printf 'Built %s, build ID %s. Cross-compiled artifacts still require target-host runtime validation.\n' "$selection" "$GRIDSTATOR_BUILD_ID"
