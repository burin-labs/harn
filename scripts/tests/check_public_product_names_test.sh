#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
script="$root/scripts/check_public_product_names.sh"

if ! "$script"; then
  echo "error: clean public contract files must pass the product-name scan" >&2
  exit 1
fi

tmp="$(mktemp "${TMPDIR:-/tmp}/harn-public-product-names-test.XXXXXX.md")"
trap 'rm -f "$tmp"' EXIT
printf '%s\n' "see burin-code#1" >"$tmp"

# Point the scanner at a fixture by swapping SECURITY.md via a copy root.
fixture_root="$(mktemp -d "${TMPDIR:-/tmp}/harn-public-product-names-fix.XXXXXX")"
trap 'rm -rf "$fixture_root" "$tmp"' EXIT
mkdir -p "$fixture_root/.github/workflows" "$fixture_root/.github/actions" "$fixture_root/scripts"
cp "$script" "$fixture_root/scripts/check_public_product_names.sh"
cp "$root/.github/SECURITY.md" "$fixture_root/.github/SECURITY.md"
printf '%s\n' "see burin-code#1" >>"$fixture_root/.github/SECURITY.md"
: >"$fixture_root/.gitignore"
if "$fixture_root/scripts/check_public_product_names.sh"; then
  echo "error: a planted downstream product name must fail the scan" >&2
  exit 1
fi

echo "check_public_product_names tests passed"
