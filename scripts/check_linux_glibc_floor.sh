#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: $0 BINARY MAX_GLIBC_VERSION" >&2
}

if [[ $# -ne 2 ]]; then
  usage
  exit 2
fi

binary="$1"
floor="$2"
if [[ ! -f "$binary" ]]; then
  echo "glibc floor check: binary does not exist: $binary" >&2
  exit 1
fi
if [[ ! "$floor" =~ ^[0-9]+\.[0-9]+$ ]]; then
  echo "glibc floor check: invalid maximum version: $floor" >&2
  exit 2
fi

readelf_bin="${HARN_READELF_BIN:-readelf}"
version_info="$("$readelf_bin" --version-info "$binary")"
versions="$({ printf '%s\n' "$version_info" \
  | grep -oE 'GLIBC_[0-9]+(\.[0-9]+)+' || true; } \
  | sed 's/^GLIBC_//' \
  | sort -Vu)"
if [[ -z "$versions" ]]; then
  echo "glibc floor check: no GLIBC symbol versions found in $binary" >&2
  exit 1
fi

observed="$(tail -n1 <<<"$versions")"
highest="$(printf '%s\n%s\n' "$floor" "$observed" | sort -V | tail -n1)"
if [[ "$highest" != "$floor" ]]; then
  echo "glibc floor check: $binary requires GLIBC_$observed, above supported maximum GLIBC_$floor" >&2
  exit 1
fi

echo "glibc floor check: $binary maximum GLIBC requirement is $observed (policy <= $floor)"
