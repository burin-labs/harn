#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

VERIFY_CLI=0
SKIP_CLI=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --verify-cli)
      VERIFY_CLI=1
      shift
      ;;
    --skip-cli)
      SKIP_CLI=1
      shift
      ;;
    *)
      echo "error: unknown arg: $1" >&2
      echo "usage: ./scripts/verify_crate_packages.sh [--verify-cli|--skip-cli]" >&2
      exit 1
      ;;
  esac
done

if [[ "$VERIFY_CLI" -eq 1 && "$SKIP_CLI" -eq 1 ]]; then
  echo "error: --verify-cli and --skip-cli are mutually exclusive" >&2
  exit 1
fi

metadata="$(cargo metadata --format-version 1 --no-deps)"
target_dir="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])' <<<"$metadata")"
stdlib_version="$(
  python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "harn-stdlib"))' \
    <<<"$metadata"
)"
modules_version="$(
  python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "harn-modules"))' \
    <<<"$metadata"
)"

echo "=== Package harn-stdlib ==="
cargo package -p harn-stdlib --allow-dirty

stdlib_crate="$target_dir/package/harn-stdlib-$stdlib_version.crate"
if [[ ! -f "$stdlib_crate" ]]; then
  echo "error: expected package archive missing: $stdlib_crate" >&2
  exit 1
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

tar -xzf "$stdlib_crate" -C "$tmp"
stdlib_pkg="$tmp/harn-stdlib-$stdlib_version"

echo "=== Inspect harn-stdlib package contents ==="
while IFS= read -r source; do
  rel="${source#crates/harn-stdlib/}"
  packaged="$stdlib_pkg/$rel"
  if [[ ! -f "$packaged" ]]; then
    echo "error: packaged harn-stdlib is missing $rel" >&2
    exit 1
  fi
  if ! cmp -s "$source" "$packaged"; then
    echo "error: packaged $rel differs from $source" >&2
    exit 1
  fi
done < <(find crates/harn-stdlib/src/stdlib -maxdepth 1 -name 'stdlib*.harn' -print | sort)

if grep -R '\.\./harn-\(vm\|modules\)' "$stdlib_pkg/src" >/dev/null; then
  echo "error: packaged harn-stdlib contains workspace-relative consumer includes" >&2
  exit 1
fi

echo "=== Check extracted harn-stdlib package ==="
CARGO_TARGET_DIR="$tmp/target-stdlib" cargo check --manifest-path "$stdlib_pkg/Cargo.toml"

# `harn-stdlib` is a new workspace dependency. Until the first release that
# publishes it, packages that depend on it cannot resolve their registry
# dependency during `cargo package`. Keep checking the owner crate above in
# bootstrap mode; after harn-stdlib exists on crates.io this consumer package
# check resumes.
if [[ "${HARN_BOOTSTRAP_NEW_CRATES:-0}" == "1" || "$SKIP_CLI" -eq 1 ]]; then
  echo "=== Skip harn-modules package check (bootstrap mode) ==="
else
  echo "=== Package harn-modules ==="
  cargo package -p harn-modules --allow-dirty --no-verify

  modules_crate="$target_dir/package/harn-modules-$modules_version.crate"
  if [[ ! -f "$modules_crate" ]]; then
    echo "error: expected package archive missing: $modules_crate" >&2
    exit 1
  fi

  tar -xzf "$modules_crate" -C "$tmp"
  modules_pkg="$tmp/harn-modules-$modules_version"

  echo "=== Inspect harn-modules package stdlib use ==="
  if [[ -e "$modules_pkg/src/stdlib" ]]; then
    echo "error: packaged harn-modules still contains a mirrored src/stdlib tree" >&2
    exit 1
  fi

  if grep -R '\.\./harn-\(vm\|stdlib\)' "$modules_pkg/src" >/dev/null; then
    echo "error: packaged harn-modules contains workspace-relative stdlib includes" >&2
    exit 1
  fi

  echo "=== Check extracted harn-modules package ==="
  CARGO_TARGET_DIR="$tmp/target-modules" cargo check --manifest-path "$modules_pkg/Cargo.toml"
fi

# `harn-hostlib` is a workspace path dep of `harn-cli`. Verifying it
# packages cleanly here (a) catches scaffold issues for the crate on its
# own and (b) mirrors the per-crate audit pattern used for harn-modules
# above. The check is independent of the harn-cli step below — packaging
# harn-hostlib does not preempt cargo's "must exist on crates.io" lookup
# for harn-cli's version requirement on harn-hostlib.
echo "=== Package harn-hostlib ==="
cargo package -p harn-hostlib --allow-dirty --no-verify

# `cargo package -p harn-cli` resolves harn-cli's path deps against
# crates.io to validate the version requirement that cargo will publish.
# When a workspace crate (e.g. harn-stdlib or harn-hostlib) was just added and has never
# been published, that lookup fails with "no matching package named X
# found" — even with --no-verify, which only skips the staged build, not
# dependency resolution. Set HARN_BOOTSTRAP_NEW_CRATES=1 (or pass
# --skip-cli) on the first release that ships such a crate; the real
# `cargo publish --workspace` later will still order intra-workspace
# deps correctly. See harn#609 for the full story.
if [[ "${HARN_BOOTSTRAP_NEW_CRATES:-0}" == "1" || "$SKIP_CLI" -eq 1 ]]; then
  echo "=== Skip harn-cli package check (bootstrap mode) ==="
  echo "Package verification complete (skipped harn-cli for new-crate bootstrap)"
  exit 0
fi

echo "=== Package harn-cli ==="
if [[ "$VERIFY_CLI" -eq 1 ]]; then
  cargo package -p harn-cli --allow-dirty
else
  cargo package -p harn-cli --allow-dirty --no-verify
fi

echo "Package verification complete"
