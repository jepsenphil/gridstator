#!/bin/sh
# Native release installer. Runtime requirements: ordinary OS tools and curl.
# This downloads a CLI only; init, join, and install-service remain explicit user actions.
set -eu
umask 077

tag=
prefix=${HOME:?HOME must identify the installing account}/.local
base_url=https://github.com/jepsenphil/gridstator/releases/download
temp_dir=
staged_binary=
lock_dir=
only_if_missing=false
bundle_dir=
trusted_manifest=
trusted_archive=

fail() { printf '%s\n' "gridstator installer: $*" >&2; exit 1; }
usage() {
    cat <<'EOF'
Usage: install-native.sh --tag v2.0.0 [--prefix /absolute/path] [--base-url URL] [--if-missing]
       install-native.sh --tag VERSION --bundle-dir /absolute/path --manifest-sha256 DIGEST --archive-sha256 DIGEST [--prefix /absolute/path] [--if-missing]

Downloads a checksum-verified native Gridstator release. Default prefix: ~/.local.
No cluster is initialized and no service is installed or started. Manual release downloads remain
available. --if-missing verifies an existing binary instead of replacing it. Check the selected
release notes for supported platforms and signing status.
EOF
}
cleanup() {
    if [ -n "$staged_binary" ]; then rm -f "$staged_binary"; fi
    if [ -n "$lock_dir" ]; then rmdir "$lock_dir" 2>/dev/null || :; fi
    if [ -n "$temp_dir" ]; then rm -rf "$temp_dir"; fi
}
trap cleanup 0
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

while [ "$#" -gt 0 ]; do
    case "$1" in
        --tag|--prefix|--base-url|--bundle-dir|--manifest-sha256|--archive-sha256)
            [ "$#" -ge 2 ] || fail "$1 requires a value"
            case "$1" in
                --tag) tag=$2 ;;
                --prefix) prefix=$2 ;;
                --base-url) base_url=$2 ;;
                --bundle-dir) bundle_dir=$2 ;;
                --manifest-sha256) trusted_manifest=$2 ;;
                --archive-sha256) trusted_archive=$2 ;;
            esac
            shift 2
            ;;
        --if-missing) only_if_missing=true; shift ;;
        --help|-h) usage; exit 0 ;;
        *) fail "unknown argument: $1" ;;
    esac
done
[ -n "$tag" ] || { usage >&2; fail "choose an explicit --tag"; }
case "$tag" in
    v[0-9]*.[0-9]*.[0-9]*) ;;
    *) fail "expected a published v-prefixed semantic version tag" ;;
esac
case "$tag" in *[!A-Za-z0-9.+-]*) fail "unsafe release tag" ;; esac
case "$prefix" in /*) ;; *) fail "--prefix must be an absolute path" ;; esac
case "$prefix$base_url" in *[[:cntrl:]]*) fail "installation path/URL cannot contain control characters" ;; esac
if [ -n "$bundle_dir" ]; then
    case "$bundle_dir" in /*) ;; *) fail "--bundle-dir must be an absolute path" ;; esac
    case "$bundle_dir" in *[[:cntrl:]]*) fail "bundle path cannot contain control characters" ;; esac
    [ "${#trusted_manifest}" -eq 64 ] || fail "offline bundles require a trusted --manifest-sha256"
    case "$trusted_manifest" in *[!a-f0-9]*) fail "invalid trusted manifest checksum" ;; esac
    [ "${#trusted_archive}" -eq 64 ] || fail "offline bundles require a trusted --archive-sha256"
    case "$trusted_archive" in *[!a-f0-9]*) fail "invalid trusted archive checksum" ;; esac
fi
base_url=${base_url%/}
case "$base_url" in
    https://*) curl_protocol='=https' ;;
    http://127.0.0.1:*|http://localhost:*) curl_protocol='=http,https' ;;
    *) fail "download base must use HTTPS (loopback HTTP is accepted for local testing)" ;;
esac
case "$base_url" in *'?'*|*'#'*|*'@'*|*' '*) fail "download base cannot contain credentials, query, fragment, or spaces" ;; esac

for tool in tar gzip awk od tr mktemp uname wc cp chmod mv mkdir rm rmdir; do
    command -v "$tool" >/dev/null 2>&1 || fail "required OS tool is missing: $tool"
done
if [ -z "$bundle_dir" ]; then
    command -v curl >/dev/null 2>&1 || fail "required OS tool is missing: curl"
fi
if command -v sha256sum >/dev/null 2>&1; then
    hash_tool=sha256sum
elif command -v shasum >/dev/null 2>&1; then
    hash_tool=shasum
else
    fail "install sha256sum or shasum to verify release downloads"
fi

case "$(uname -s)" in Linux) release_os=linux ;; Darwin) release_os=darwin ;; *) fail "supported hosts are Linux/WSL2 and macOS" ;; esac
case "$(uname -m)" in x86_64|amd64) release_arch=amd64 ;; aarch64|arm64) release_arch=arm64 ;; *) fail "supported architectures are x86-64 and ARM64" ;; esac
target=$release_os/$release_arch
version=${tag#v}
archive_name=gridstator-$version-$release_os-$release_arch.tar.gz
manifest_name=gridstator-release.json
temp_dir=$(mktemp -d "${TMPDIR:-/tmp}/gridstator-install.XXXXXXXX") || fail "cannot create private download directory"

download() {
    download_name=$1
    download_limit=$2
    if [ -n "$bundle_dir" ]; then
        [ -f "$bundle_dir/$download_name" ] && [ ! -L "$bundle_dir/$download_name" ] || fail "missing regular bundled file: $download_name"
        bundled_size=$(wc -c < "$bundle_dir/$download_name")
        [ "$bundled_size" -le "$download_limit" ] || fail "$download_name exceeded its size limit"
        cp "$bundle_dir/$download_name" "$temp_dir/$download_name" || fail "could not read bundled $download_name"
    else
        curl --fail --location --silent --show-error --proto "$curl_protocol" --proto-redir '=https' \
            --tlsv1.2 --connect-timeout 15 --max-time 300 --max-filesize "$download_limit" --output "$temp_dir/$download_name" \
            "$base_url/$tag/$download_name" || fail "could not download $download_name"
    fi
    actual_size=$(wc -c < "$temp_dir/$download_name")
    [ "$actual_size" -le "$download_limit" ] || fail "$download_name exceeded its size limit"
}
expected_checksum() {
    awk -v wanted="$1" '
        NF == 2 && ($2 == wanted || $2 == "*" wanted) {
            if (length($1) != 64 || $1 !~ /^[0-9a-fA-F]+$/) bad = 1
            count++; checksum = tolower($1)
        }
        END { if (count != 1 || bad) exit 1; print checksum }
    ' "$temp_dir/SHA256SUMS"
}
verify_checksum() {
    checksum_name=$1
    expected=$(expected_checksum "$checksum_name") || fail "missing, invalid, or ambiguous checksum for $checksum_name"
    if [ "$hash_tool" = sha256sum ]; then
        actual=$(sha256sum "$temp_dir/$checksum_name") || fail "cannot hash $checksum_name"
    else
        actual=$(shasum -a 256 "$temp_dir/$checksum_name") || fail "cannot hash $checksum_name"
    fi
    actual=${actual%% *}
    [ "$actual" = "$expected" ] || fail "checksum mismatch for $checksum_name"
}

printf 'Preparing verified native Gridstator release %s for %s\n' "$tag" "$target" >&2
download SHA256SUMS 1048576
download "$manifest_name" 1048576
if [ -n "$bundle_dir" ]; then
    if [ "$hash_tool" = sha256sum ]; then
        pinned_actual=$(sha256sum "$temp_dir/$manifest_name")
    else
        pinned_actual=$(shasum -a 256 "$temp_dir/$manifest_name")
    fi
    pinned_actual=${pinned_actual%% *}
    [ "$pinned_actual" = "$trusted_manifest" ] || fail "trusted manifest checksum mismatch"
fi
verify_checksum "$manifest_name"
manifest_checksum=$(expected_checksum "$manifest_name")
download "$archive_name" 536870912
verify_checksum "$archive_name"
if [ -n "$bundle_dir" ]; then
    [ "$(expected_checksum "$archive_name")" = "$trusted_archive" ] || fail "trusted archive checksum mismatch"
fi

# Stream one known regular member to a private file. Never unpack archive paths or preserve archive
# owners/modes. Reject duplicate/symlink entries before considering the staged bytes executable.
tar -tzf "$temp_dir/$archive_name" > "$temp_dir/members" || fail "cannot list release archive"
member_count=$(awk '$0 == "gridstator" { count++ } END { print count + 0 }' "$temp_dir/members")
[ "$member_count" -eq 1 ] || fail "archive must contain exactly one gridstator entry"
tar -tvzf "$temp_dir/$archive_name" gridstator > "$temp_dir/member-type" || fail "cannot inspect executable archive entry"
awk 'NR != 1 || substr($0, 1, 1) != "-" { bad = 1 } END { exit bad || NR != 1 }' "$temp_dir/member-type" || fail "gridstator archive entry must be a regular file"
tar -xOzf "$temp_dir/$archive_name" gridstator > "$temp_dir/gridstator" || fail "cannot read executable archive entry"

# An archive can have the right published checksum and still be a packaging mistake. Check the
# target header before executing its manifest-validation helper; full executable digests follow.
magic=$(od -An -tx1 -N4 "$temp_dir/gridstator" | tr -d '[:space:]')
case "$target" in
    linux/*)
        class=$(od -An -tx1 -j4 -N2 "$temp_dir/gridstator" | tr -d '[:space:]')
        machine=$(od -An -tx1 -j18 -N2 "$temp_dir/gridstator" | tr -d '[:space:]')
        [ "$magic" = 7f454c46 ] && [ "$class" = 0201 ] || fail "download is not a native 64-bit Linux executable"
        case "$target:$machine" in linux/amd64:3e00|linux/arm64:b700) ;; *) fail "executable has the wrong architecture" ;; esac
        ;;
    darwin/*)
        machine=$(od -An -tx1 -j4 -N4 "$temp_dir/gridstator" | tr -d '[:space:]')
        [ "$magic" = cffaedfe ] || fail "download is not a native 64-bit macOS executable"
        case "$target:$machine" in darwin/amd64:07000001|darwin/arm64:0c000001) ;; *) fail "executable has the wrong architecture" ;; esac
        ;;
esac
chmod 700 "$temp_dir/gridstator"
verify_native() {
    "$temp_dir/gridstator" release verify --manifest "$temp_dir/$manifest_name" \
        --manifest-sha256 "$manifest_checksum" --archive "$temp_dir/$archive_name" \
        --executable "$1" --target "$target" > "$temp_dir/verification.json"
}
verify_native "$temp_dir/gridstator" || fail "native release manifest/executable verification failed"

mkdir -p "$prefix/bin" "$prefix/share/gridstator/releases"
lock_dir=$prefix/share/gridstator/.installer-lock
if ! mkdir "$lock_dir" 2>/dev/null; then
    lock_dir=
    fail "another installation or an abandoned installer lock exists at $prefix/share/gridstator/.installer-lock"
fi
release_dir=$prefix/share/gridstator/releases/$tag
if [ -e "$release_dir/$manifest_name" ]; then
    if [ "$hash_tool" = sha256sum ]; then
        recorded=$(sha256sum "$release_dir/$manifest_name") || fail "cannot inspect existing release metadata"
    else
        recorded=$(shasum -a 256 "$release_dir/$manifest_name") || fail "cannot inspect existing release metadata"
    fi
    recorded=${recorded%% *}
    [ "$recorded" = "$manifest_checksum" ] || fail "this release tag already has different immutable metadata"
fi
[ ! -d "$prefix/bin/gridstator" ] || fail "$prefix/bin/gridstator is a directory"
reuse_existing=false
if [ "$only_if_missing" = true ] && { [ -e "$prefix/bin/gridstator" ] || [ -L "$prefix/bin/gridstator" ]; }; then
    # The downloaded, verified helper reads the candidate as data; the existing file is never
    # executed until its bytes have been checked against this exact official release.
    verify_native "$prefix/bin/gridstator" || fail "existing CLI does not match the selected verified release; left unchanged"
    reuse_existing=true
fi
mkdir -p "$release_dir"
cp "$temp_dir/$manifest_name" "$release_dir/$manifest_name"
cp "$temp_dir/SHA256SUMS" "$release_dir/SHA256SUMS"
cp "$temp_dir/verification.json" "$release_dir/verification.json"
if [ "$reuse_existing" = true ]; then
    printf 'Verified existing %s/bin/gridstator; left unchanged\nVerified release metadata: %s\n' "$prefix" "$release_dir"
    exit 0
fi
staged_binary=$(mktemp "$prefix/bin/.gridstator.XXXXXXXX") || fail "cannot stage installation on the destination filesystem"
cp "$temp_dir/gridstator" "$staged_binary"
chmod 755 "$staged_binary"
verify_native "$staged_binary" || fail "staged installation does not match its verified release"
mv -f "$staged_binary" "$prefix/bin/gridstator"
staged_binary=

printf 'Installed %s/bin/gridstator\nVerified release metadata: %s\n' "$prefix" "$release_dir"
printf '%s\n' 'Review the release notes for platform support and signing status.'
for worker_tool in docker act git; do
    if ! command -v "$worker_tool" >/dev/null 2>&1; then
        printf 'Worker prerequisite missing from PATH: %s\n' "$worker_tool" >&2
    fi
done
if command -v docker >/dev/null 2>&1 && ! docker buildx version >/dev/null 2>&1; then
    printf '%s\n' 'Docker buildx is unavailable; image-build workers require it.' >&2
fi
printf '%s\n' 'Next: add the prefix bin directory to PATH, then choose explicit setup commands:'
printf '%s\n' '  gridstator controller init' '  gridstator controller install-service' '  gridstator agent join <controller> --token <join-token> --fingerprint <ca-fingerprint>' '  gridstator agent install-service'
