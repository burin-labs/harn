#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$repo_root"

persona_asset="crates/harn-cli/assets/persona-templates/deterministic-sweeper/prompts/system.harn.prompt"
portal_dist="crates/harn-cli/portal-dist"
portal_asset="$portal_dist/assets/portal/app.js"

if [[ ! -f "$persona_asset" ]]; then
  echo "missing embedded persona asset: $persona_asset" >&2
  exit 1
fi
if ! git diff --quiet -- "$persona_asset" || ! git diff --cached --quiet -- "$persona_asset"; then
  echo "embedded asset rebuild test requires an unmodified persona fixture" >&2
  exit 1
fi

tmp_root=$(mktemp -d)
persona_backup="$tmp_root/persona-asset"
portal_backup="$tmp_root/portal-dist"
portal_dist_existed=0

cp -p "$persona_asset" "$persona_backup"
if [[ -d "$portal_dist" ]]; then
  portal_dist_existed=1
  cp -pR "$portal_dist" "$portal_backup"
fi

cleanup() {
  local status=$?
  cp -p "$persona_backup" "$persona_asset"
  rm -rf "$portal_dist"
  if [[ "$portal_dist_existed" -eq 1 ]]; then
    cp -pR "$portal_backup" "$portal_dist"
  fi
  rm -rf "$tmp_root"
  exit "$status"
}
trap cleanup EXIT

run_check() {
  local log_path=$1
  shift
  HARN_DISABLE_AUTO_HOOK_SETUP=1 \
    CARGO_TARGET_DIR="$tmp_root/target" \
    CARGO_BUILD_BUILD_DIR="$tmp_root/build" \
    "$repo_root/scripts/cargo_with_worktree_build_dir.sh" "$@" -p harn-cli --bin harn \
    >"$log_path" 2>&1
}

assert_rebuilt_for() {
  local label=$1
  local log_path=$2
  local asset=$3
  if ! grep -Fq "Dirty harn-cli" "$log_path"; then
    echo "$label asset change did not dirty harn-cli" >&2
    cat "$log_path" >&2
    exit 1
  fi
  if ! grep -Fq "$asset" "$log_path"; then
    echo "$label rebuild did not cite the changed asset: $asset" >&2
    cat "$log_path" >&2
    exit 1
  fi
}

# Cargo tracks rerun-if-changed inputs by mtime. Pin the baseline fixture to a
# past timestamp so the mutation is distinguishable even on coarse filesystems.
touch -t 200001010101 "$persona_asset"
run_check "$tmp_root/persona-baseline.log" check --quiet
sleep 1
printf '\n# embedded asset rebuild test marker\n' >> "$persona_asset"
run_check "$tmp_root/persona-dirty.log" -vv check
assert_rebuilt_for "persona template" "$tmp_root/persona-dirty.log" "$persona_asset"

# Refresh after restoration before exercising the second root. Without this,
# Cargo could cite the first restoration and falsely attribute the dirty build
# to the portal asset.
cp -p "$persona_backup" "$persona_asset"
run_check "$tmp_root/persona-restored.log" check --quiet

mkdir -p "$(dirname "$portal_asset")"
if [[ ! -f "$portal_asset" ]]; then
  printf '%s\n' '// portal fallback asset for embedded watcher coverage' > "$portal_asset"
fi
run_check "$tmp_root/portal-baseline.log" check --quiet
sleep 1
printf '\n// embedded asset rebuild test marker\n' >> "$portal_asset"
run_check "$tmp_root/portal-dirty.log" -vv check
assert_rebuilt_for "portal asset" "$tmp_root/portal-dirty.log" "$portal_asset"

cp -p "$persona_backup" "$persona_asset"
if ! git diff --quiet -- "$persona_asset"; then
  echo "embedded asset rebuild test did not restore $persona_asset" >&2
  exit 1
fi

echo "embedded_asset_rebuild_test: ok"
