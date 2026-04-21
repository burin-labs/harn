#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

VERIFY_CLI=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --verify-cli)
      VERIFY_CLI=1
      shift
      ;;
    *)
      echo "error: unknown arg: $1" >&2
      echo "usage: ./scripts/verify_crate_packages.sh [--verify-cli]" >&2
      exit 1
      ;;
  esac
done

metadata="$(cargo metadata --format-version 1 --no-deps)"
target_dir="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])' <<<"$metadata")"
modules_version="$(
  python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "harn-modules"))' \
    <<<"$metadata"
)"

echo "=== Package harn-modules ==="
cargo package -p harn-modules --allow-dirty

modules_crate="$target_dir/package/harn-modules-$modules_version.crate"
if [[ ! -f "$modules_crate" ]]; then
  echo "error: expected package archive missing: $modules_crate" >&2
  exit 1
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

tar -xzf "$modules_crate" -C "$tmp"
modules_pkg="$tmp/harn-modules-$modules_version"

echo "=== Inspect harn-modules package stdlib mirror ==="
while IFS= read -r vm_source; do
  rel="${vm_source#crates/harn-vm/src/}"
  packaged="$modules_pkg/src/stdlib/$rel"
  if [[ ! -f "$packaged" ]]; then
    echo "error: packaged harn-modules is missing src/stdlib/$rel" >&2
    exit 1
  fi
  if ! cmp -s "$vm_source" "$packaged"; then
    echo "error: packaged src/stdlib/$rel differs from $vm_source" >&2
    exit 1
  fi
done < <(find crates/harn-vm/src -maxdepth 1 -name 'stdlib*.harn' -print | sort)

if grep -R '\.\./harn-vm' "$modules_pkg/src" >/dev/null; then
  echo "error: packaged harn-modules still contains workspace-relative harn-vm includes" >&2
  exit 1
fi

echo "=== Check extracted harn-modules package ==="
CARGO_TARGET_DIR="$tmp/target" cargo check --manifest-path "$modules_pkg/Cargo.toml"

echo "=== Package harn-cli ==="
if [[ "$VERIFY_CLI" -eq 1 ]]; then
  cargo package -p harn-cli --allow-dirty
else
  cargo package -p harn-cli --allow-dirty --no-verify
fi

echo "Package verification complete"
