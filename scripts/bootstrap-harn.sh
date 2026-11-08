#!/bin/sh
# Stage zero only: fetch one pinned, verified seed; Harn owns runtime installation.
set -eu
root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
export HARN_EXT_BOOTSTRAP_TOKEN="${HARN_BOOTSTRAP_TOKEN:-${HARN_EXT_BOOTSTRAP_TOKEN:-}}"
export HARN_EXT_BOOTSTRAP_OFFLINE="${HARN_BOOTSTRAP_OFFLINE:-${HARN_EXT_BOOTSTRAP_OFFLINE:-0}}"
export HARN_BOOTSTRAP_REPOSITORY="${HARN_BOOTSTRAP_REPOSITORY:-burin-labs/harn}"
unset HARN_BOOTSTRAP_TOKEN HARN_BOOTSTRAP_OFFLINE
case "$(printf '%s' "$HARN_EXT_BOOTSTRAP_OFFLINE" | tr '[:upper:]' '[:lower:]')" in 1|true) HARN_EXT_BOOTSTRAP_OFFLINE=1;; esac
for argument do
  if [ "$argument" = --offline ]; then HARN_EXT_BOOTSTRAP_OFFLINE=1; fi
done
run_seed() {
  if [ "${1:-}" = --seed-command ]; then
    shift
    "$HARN_EXT_BOOTSTRAP_INSTALLER" "$@"
  else
    "$HARN_EXT_BOOTSTRAP_INSTALLER" run --standalone --no-sandbox "$root/scripts/bootstrap-harn.harn" -- "$@"
  fi
}
if [ -n "${HARN_EXT_BOOTSTRAP_INSTALLER:-}" ]; then
  run_seed "$@"
  exit
fi
version=${HARN_EXT_BOOTSTRAP_SEED_VERSION:-}
if [ -z "$version" ]; then
  version=$(cat "$root/.harn-bootstrap-version")
fi
printf '%s\n' "$version" | grep -Eq '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$' || { echo 'invalid pinned bootstrap seed version' >&2; exit 1; }
case "$(uname -m)" in arm64|aarch64) arch=aarch64;; x86_64|amd64) arch=x86_64;; *) echo 'unsupported seed architecture' >&2; exit 1;; esac
# Releases before XZ_FIRST_RELEASE ship .tar.gz; later ones ship .tar.xz.
XZ_FIRST_RELEASE=0.10.179
unix_extension=tar.gz
[ "$(printf '%s\n%s\n' "$version" "$XZ_FIRST_RELEASE" | sort -V | head -n 1)" != "$XZ_FIRST_RELEASE" ] || unix_extension=tar.xz
case "$(uname -s)" in
  Darwin) target=$arch-apple-darwin; extension=$unix_extension; name=harn;;
  Linux) target=$arch-unknown-linux-gnu; extension=$unix_extension; name=harn;;
  MINGW*|MSYS*|CYGWIN*) target=x86_64-pc-windows-msvc; extension=zip; name=harn.exe;;
  *) echo 'unsupported seed platform' >&2; exit 1;;
esac
cache=${HARN_EXT_BOOTSTRAP_SEED_CACHE:-${XDG_CACHE_HOME:-${HOME}/.cache}/harn/bootstrap-seed}
mkdir -p "$cache/$version/$target"
cache=$cache/$version/$target
work=$(mktemp -d "${TMPDIR:-/tmp}/harn-seed.XXXXXXXX")
asset=harn-$target.$extension
manifest_tmp=$cache/SHA256SUMS.tmp.$$
archive_tmp=$cache/$asset.tmp.$$
cleanup() {
  status=$?
  trap - EXIT HUP INT TERM
  # Windows may keep the seed executable locked after it exits. Cleanup must
  # preserve the install/verification result, including failures.
  if ! rm -rf "$work"; then
    printf 'warning: bootstrap seed cleanup left %s\n' "$work" >&2
  fi
  if ! rm -f "$manifest_tmp" "$archive_tmp"; then
    printf 'warning: bootstrap cache cleanup left %s or %s\n' "$manifest_tmp" "$archive_tmp" >&2
  fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
base=https://github.com/burin-labs/harn/releases/download/v$version
api=https://api.github.com/repos/burin-labs/harn/releases
api_mode=0
fetch() {
  case "$1" in
    https://api.github.com/*)
      if [ -n "$HARN_EXT_BOOTSTRAP_TOKEN" ]; then
        # Keep credentials out of process arguments and restrict header files
        # to this private seed workspace.
        if [ "$(printf '%s' "$HARN_EXT_BOOTSTRAP_TOKEN" | LC_ALL=C tr -d '[:cntrl:]')" != "$HARN_EXT_BOOTSTRAP_TOKEN" ]; then
          echo 'invalid bootstrap token' >&2
          return 1
        fi
        if command -v curl >/dev/null 2>&1; then
          (umask 077; printf 'Authorization: Bearer %s\n' "$HARN_EXT_BOOTSTRAP_TOKEN" > "$work/api.headers")
          if curl -q -fLsS --connect-timeout 15 --max-time 120 \
            -H "@$work/api.headers" -H "Accept: $3" "$1" -o "$2"; then
            api_status=0
          else
            api_status=$?
          fi
          rm -f "$work/api.headers"
          return "$api_status"
        fi
        (umask 077; printf 'header = Authorization: Bearer %s\n' "$HARN_EXT_BOOTSTRAP_TOKEN" > "$work/api.wget")
        # Wget custom headers survive redirects. Stop at the API response and
        # follow an HTTPS asset redirect in a separate, unauthenticated request.
        if wget --config="$work/api.wget" --server-response \
          --max-redirect=0 --tries=1 --timeout=120 --header="Accept: $3" \
          "$1" -O "$2" 2> "$work/api.response"; then
          rm -f "$work/api.wget"
          return 0
        fi
        rm -f "$work/api.wget"
        redirect=$(awk '
          $1 ~ /^HTTP\/[0-9.]+$/ { status = $2; location = ""; count = 0 }
          /^[[:space:]]/ && tolower($1) == "location:" { sub(/\r$/, "", $2); location = $2; count++ }
          END { if (status ~ /^30[12378]$/ && count == 1) print location; else exit 1 }
        ' "$work/api.response") || { echo 'release asset API request failed' >&2; return 1; }
        case "$redirect" in
          https://*) ;;
          *) echo 'release asset API returned an invalid redirect' >&2; return 1 ;;
        esac
        wget --no-config -q --tries=1 --timeout=120 --header="Accept: $3" "$redirect" -O "$2"
        return
      fi
      ;;
  esac
  if command -v curl >/dev/null 2>&1; then
    curl -q -fLsS --connect-timeout 15 --max-time 120 -H "Accept: $3" "$1" -o "$2"
  else
    wget --no-config -q --tries=1 --timeout=120 --header="Accept: $3" "$1" -O "$2"
  fi
}
download() {
  if [ "$api_mode" = 0 ] && [ -z "$HARN_EXT_BOOTSTRAP_TOKEN" ] \
    && fetch "$1" "$2" application/octet-stream; then
    return 0
  fi
  if [ "$api_mode" = 0 ]; then
    command -v jq >/dev/null 2>&1 || { echo 'release download failed; API fallback requires jq' >&2; return 1; }
    echo 'fetching bootstrap seed through the GitHub release asset API' >&2
    fetch "$api/tags/v$version" "$work/release.json" application/vnd.github+json || return 1
    api_mode=1
  fi
  id=$(jq -er --arg name "${1##*/}" '
    [.assets[] | select(.name == $name)]
    | if length == 1 and (.[0].id | type) == "number"
      then .[0].id else error("expected one release asset") end
  ' "$work/release.json") || return 1
  case "$id" in ''|0|*[!0-9]*) echo 'invalid release asset ID' >&2; return 1;; esac
  fetch "$api/assets/$id" "$2" application/octet-stream
}
digest() {
  # Hash standard input: with a file name, sha256sum escapes a Windows path and
  # prefixes the digest with a backslash.
  if command -v sha256sum >/dev/null 2>&1; then result=$(sha256sum < "$1") || return
  else result=$(shasum -a 256 < "$1") || return; fi
  printf '%s\n' "${result%% *}"
}
seed_checksum() {
  awk -v asset="$asset" '
  NF == 0 { next }
  {
    checksum = $1
    name = $2
    sub(/^\*/, "", name)
    remainder = checksum
    gsub(/[0-9a-fA-F]/, "", remainder)
    if (NF != 2 || length(checksum) != 64 || remainder != "" || name == "" || name ~ /[\\\/]/ || seen[name]++) exit 1
    if (name == asset) { count++; selected = checksum }
  }
  END { if (count != 1) exit 1; print selected }
' "$1"
}
cached_verified=0
if [ -f "$cache/SHA256SUMS" ] && [ -f "$cache/$asset" ]; then
  cached_expected=$(seed_checksum "$cache/SHA256SUMS") || cached_expected=''
  cached_expected=$(printf '%s' "$cached_expected" | tr A-F a-f)
  if [ -n "$cached_expected" ] && [ "$(digest "$cache/$asset")" = "$cached_expected" ]; then
    cached_verified=1
  fi
fi
manifest_fetched=0
if [ "$HARN_EXT_BOOTSTRAP_OFFLINE" != 1 ] && [ "$cached_verified" != 1 ]; then
  download "$base/SHA256SUMS" "$work/SHA256SUMS"
  manifest_source=$work/SHA256SUMS
  manifest_fetched=1
else
  [ -f "$cache/SHA256SUMS" ] || { echo 'offline seed manifest missing' >&2; exit 1; }
  manifest_source=$cache/SHA256SUMS
fi
expected=$(seed_checksum "$manifest_source") || { echo 'invalid seed checksum manifest' >&2; exit 1; }
expected=$(printf '%s' "$expected" | tr A-F a-f)
if [ "$manifest_fetched" = 1 ]; then
  if [ -f "$cache/SHA256SUMS" ]; then
    cmp -s "$work/SHA256SUMS" "$cache/SHA256SUMS" || { echo 'published seed checksums changed' >&2; exit 1; }
  else
    cp "$work/SHA256SUMS" "$manifest_tmp"
    if ln "$manifest_tmp" "$cache/SHA256SUMS" 2>/dev/null; then
      :
    elif [ ! -f "$cache/SHA256SUMS" ] \
      || ! cmp -s "$manifest_tmp" "$cache/SHA256SUMS"; then
      echo 'concurrent seed checksum publication changed' >&2
      exit 1
    fi
  fi
fi
actual=''
downloaded=0
if [ -f "$cache/$asset" ]; then cp "$cache/$asset" "$work/$asset"; actual=$(digest "$work/$asset"); fi
if [ "$actual" != "$expected" ]; then
  [ "$HARN_EXT_BOOTSTRAP_OFFLINE" != 1 ] || { echo 'offline seed archive unavailable or corrupt' >&2; exit 1; }
  download "$base/$asset" "$work/$asset"
  actual=$(digest "$work/$asset")
  downloaded=1
fi
[ "$actual" = "$expected" ] || { echo 'seed checksum mismatch' >&2; exit 1; }
if [ "$extension" = zip ]; then
  # shellcheck disable=SC2016 # PowerShell expands these variables.
  HARN_EXT_SEED_ARCHIVE="$work/$asset" HARN_EXT_SEED_BINARY="$work/$name" powershell.exe -NoProfile -NonInteractive -Command \
    'Add-Type -AssemblyName System.IO.Compression.FileSystem; $zip=[IO.Compression.ZipFile]::OpenRead($env:HARN_EXT_SEED_ARCHIVE); try { $entry=$zip.GetEntry("harn.exe"); [IO.Compression.ZipFileExtensions]::ExtractToFile($entry,$env:HARN_EXT_SEED_BINARY) } finally { $zip.Dispose() }'
else
  tar -xf "$work/$asset" -C "$work" "$name"
fi
chmod +x "$work/$name"
if [ "$downloaded" = 1 ]; then
  cp "$work/$asset" "$archive_tmp"
  mv "$archive_tmp" "$cache/$asset"
fi
export HARN_EXT_BOOTSTRAP_INSTALLER="$work/$name"
run_seed "$@"
