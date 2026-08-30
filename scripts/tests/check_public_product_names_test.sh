#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
script="$root/scripts/check_public_product_names.sh"

if ! "$script"; then
  echo "error: clean public contract files must pass the product-name scan" >&2
  exit 1
fi

fixture_root="$(mktemp -d "${TMPDIR:-/tmp}/harn-public-product-names-fix.XXXXXX")"
trap 'rm -rf "$fixture_root"' EXIT
mkdir -p "$fixture_root/docs" "$fixture_root/scripts"
cp "$script" "$fixture_root/scripts/check_public_product_names.sh"
git -C "$fixture_root" init -q
product="burin"
printf 'see %s-code#1\n' "$product" >"$fixture_root/docs/public.md"
git -C "$fixture_root" add docs/public.md scripts/check_public_product_names.sh
if "$fixture_root/scripts/check_public_product_names.sh"; then
  echo "error: a planted downstream product name must fail the scan" >&2
  exit 1
fi

git -C "$fixture_root" reset -q
rm -f "$fixture_root/docs/public.md"
printf 'historical %s-code#1\n' "$product" >"$fixture_root/CHANGELOG.md"
git -C "$fixture_root" add CHANGELOG.md scripts/check_public_product_names.sh
if ! "$fixture_root/scripts/check_public_product_names.sh"; then
  echo "error: immutable changelog history must remain allowlisted" >&2
  exit 1
fi

echo "check_public_product_names tests passed"
