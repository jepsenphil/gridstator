#!/usr/bin/env bash
set -euo pipefail

if [[ $# != 3 ]]; then
  echo 'usage: audit-native.sh <binary> <release-key> <evidence-directory>' >&2
  exit 2
fi
binary=$1
release_key=$2
evidence_dir=$3
mkdir -p "$evidence_dir"
case "$release_key" in
  linux/amd64) expected_host=x86_64-unknown-linux-gnu; expected_machine=x86_64; expected_os=Linux ;;
  linux/arm64) expected_host=aarch64-unknown-linux-gnu; expected_machine=aarch64; expected_os=Linux ;;
  darwin/amd64) expected_host=x86_64-apple-darwin; expected_machine=x86_64; expected_os=Darwin ;;
  darwin/arm64) expected_host=aarch64-apple-darwin; expected_machine=arm64; expected_os=Darwin ;;
  *) echo "Unsupported release key: $release_key" >&2; exit 2 ;;
esac
host=$(rustc -vV | sed -n 's/^host: //p')
if [[ $host != "$expected_host" || $(uname -s) != "$expected_os" || $(uname -m) != "$expected_machine" ]]; then
  echo "Actual-host proof requires $release_key ($expected_host); observed $host / $(uname -s) / $(uname -m)" >&2
  exit 1
fi
{
  echo 'proof-kind=actual-host-build-unit-cli-smoke-and-linkage'
  echo "release-key=$release_key"
  echo "rust-target=$expected_host"
  echo "source-revision=$(git rev-parse HEAD)"
  echo 'working-tree-status:'
  git status --short --untracked-files=normal
  echo "binary=$binary"
  uname -a
  rustc -vV
  cargo --version
  file "$binary"
  echo 'scope-limit=Does not prove clean-target installation, Docker, services, updates, reboot, WSL2, signing or notarization.'
} > "$evidence_dir/environment.txt"
repo_root=$(git rev-parse --show-toplevel)
(
  cd "$repo_root"
  git ls-files -z --cached --others --exclude-standard -- \
    native/Cargo.toml native/Cargo.lock native/rust-toolchain.toml native/src native/tests native/scripts native/contracts |
    while IFS= read -r -d '' source; do
      if [[ $expected_os == Linux ]]; then
        sha256sum "$source"
      else
        shasum -a 256 "$source"
      fi
    done
) > "$evidence_dir/native-source-SHA256SUMS"
"$binary" --version > "$evidence_dir/version.txt"
"$binary" --help > "$evidence_dir/help.txt"
"$binary" build-info --json > "$evidence_dir/build-info.json"
reported_target=$(sed -n 's/.*"target"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$evidence_dir/build-info.json")
if [[ $reported_target != "$release_key" ]]; then
  echo "The executing artifact reports $reported_target; expected $release_key" >&2
  exit 1
fi
if [[ $expected_os == Linux ]]; then
  sha256sum "$binary" > "$evidence_dir/SHA256SUMS"
  if ! ldd "$binary" > "$evidence_dir/linkage.txt" 2>&1; then
    if ! grep -Eq 'not a dynamic executable|statically linked' "$evidence_dir/linkage.txt"; then
      cat "$evidence_dir/linkage.txt" >&2
      exit 1
    fi
  fi
else
  sw_vers >> "$evidence_dir/environment.txt"
  shasum -a 256 "$binary" > "$evidence_dir/SHA256SUMS"
  otool -L "$binary" > "$evidence_dir/linkage.txt"
fi
if grep -Eiq 'lib(sqlite3|ssl|crypto|node)[.-]' "$evidence_dir/linkage.txt"; then
  echo 'Unexpected external SQLite/OpenSSL/Node linkage:' >&2
  cat "$evidence_dir/linkage.txt" >&2
  exit 1
fi
cat "$evidence_dir/environment.txt" "$evidence_dir/SHA256SUMS" "$evidence_dir/linkage.txt"
