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
target_dir="$tmp_root/target"
build_dir="$tmp_root/build"

# The post-warm gate already owns an executable built from this worktree. Reuse
# that target when possible so this causality proof only recompiles harn-cli
# after each mutation. Direct callers still get a private target/build pair.
if [[ "${HARN_BIN:-}" == "$repo_root/target/"* && -x "$HARN_BIN" ]]; then
  target_dir=$(cd "$(dirname "$HARN_BIN")/.." && pwd)
  build_dir=""
fi

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
  local -a cargo_env=(
    HARN_DISABLE_AUTO_HOOK_SETUP=1
    CARGO_TERM_COLOR=never
    "CARGO_TARGET_DIR=$target_dir"
  )
  if [[ -n "$build_dir" ]]; then
    cargo_env+=("CARGO_BUILD_BUILD_DIR=$build_dir")
  fi
  env "${cargo_env[@]}" "$repo_root/scripts/cargo_with_worktree_build_dir.sh" "$@" -p harn-cli --bin harn \
    >"$log_path" 2>&1
}

assert_rebuilt_for() {
  local label=$1
  local log_path=$2
  local watched_root=$3
  if ! grep -Fq "Dirty harn-cli" "$log_path"; then
    echo "$label asset change did not dirty harn-cli" >&2
    cat "$log_path" >&2
    exit 1
  fi
  if ! grep -Fq "$watched_root" "$log_path"; then
    echo "$label rebuild did not cite the watched root: $watched_root" >&2
    cat "$log_path" >&2
    exit 1
  fi
}

# Materialize both watched roots before the single baseline build. Cargo tracks
# rerun-if-changed inputs by mtime, so pin each fixture to the past and make the
# later mutations distinguishable even on coarse filesystems.
mkdir -p "$(dirname "$portal_asset")"
if [[ ! -f "$portal_asset" ]]; then
  printf '%s\n' '// portal fallback asset for embedded watcher coverage' > "$portal_asset"
fi
touch -t 200001010101 "$persona_asset" "$portal_asset"
run_check "$tmp_root/baseline.log" check --quiet

printf '\n# embedded asset rebuild test marker\n' >> "$persona_asset"
run_check "$tmp_root/persona-dirty.log" -vv check
assert_rebuilt_for \
  "persona template" \
  "$tmp_root/persona-dirty.log" \
  "crates/harn-cli/assets/persona-templates"

# Leave the persona mutation in place: its fingerprint is now current, so the
# next Cargo check can attribute the only new change to the portal root without
# paying for a separate restoration build.
printf '\n// embedded asset rebuild test marker\n' >> "$portal_asset"
run_check "$tmp_root/portal-dirty.log" -vv check
assert_rebuilt_for "portal asset" "$tmp_root/portal-dirty.log" "$portal_dist"

cp -p "$persona_backup" "$persona_asset"
if ! git diff --quiet -- "$persona_asset"; then
  echo "embedded asset rebuild test did not restore $persona_asset" >&2
  exit 1
fi

echo "embedded_asset_rebuild_test: ok"
