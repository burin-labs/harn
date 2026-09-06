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
case "$(uname -s)" in
  Darwin) target=$arch-apple-darwin; extension=tar.gz; name=harn;;
  Linux) target=$arch-unknown-linux-gnu; extension=tar.gz; name=harn;;
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
trap 'rm -rf "$work"; rm -f "$manifest_tmp" "$archive_tmp"' EXIT HUP INT TERM
base=https://github.com/burin-labs/harn/releases/download/v$version
download() {
  if command -v curl >/dev/null 2>&1; then curl -fLsS --retry 3 --retry-all-errors --connect-timeout 15 --max-time 120 "$1" -o "$2"
  else wget -q --tries=3 --timeout=120 "$1" -O "$2"; fi
}
digest() {
  if command -v sha256sum >/dev/null 2>&1; then result=$(sha256sum "$1") || return
  else result=$(shasum -a 256 "$1") || return; fi
  printf '%s\n' "${result%% *}"
}
if [ "$HARN_EXT_BOOTSTRAP_OFFLINE" != 1 ]; then
  download "$base/SHA256SUMS" "$work/SHA256SUMS"
  manifest_source=$work/SHA256SUMS
else
  [ -f "$cache/SHA256SUMS" ] || { echo 'offline seed manifest missing' >&2; exit 1; }
  manifest_source=$cache/SHA256SUMS
fi
expected=$(awk -v asset="$asset" '
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
' "$manifest_source") || { echo 'invalid seed checksum manifest' >&2; exit 1; }
expected=$(printf '%s' "$expected" | tr A-F a-f)
if [ "$HARN_EXT_BOOTSTRAP_OFFLINE" != 1 ]; then
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
  tar -xzf "$work/$asset" -C "$work" "$name"
fi
chmod +x "$work/$name"
if [ "$downloaded" = 1 ]; then
  cp "$work/$asset" "$archive_tmp"
  mv "$archive_tmp" "$cache/$asset"
fi
export HARN_EXT_BOOTSTRAP_INSTALLER="$work/$name"
run_seed "$@"
