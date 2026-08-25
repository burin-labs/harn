#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
watch_module="$repo_root/crates/harn-cli/build_support/embedded_assets.rs"

tmp_root=$(mktemp -d)
cleanup() {
  local status=$?
  if [[ "$status" -ne 0 ]]; then
    for log in "$tmp_root"/*.log; do
      [[ -f "$log" ]] || continue
      echo "--- $(basename "$log") ---" >&2
      cat "$log" >&2
    done
  fi
  rm -rf "$tmp_root"
  exit "$status"
}
trap cleanup EXIT

fixture="$tmp_root/fixture"
target_dir="$tmp_root/target"
persona_root="$fixture/assets/persona-templates"
portal_root="$fixture/portal-dist"
persona_asset="$persona_root/example/system.harn.prompt"
portal_asset="$portal_root/assets/portal/app.js"

mkdir -p "$fixture/src" "$(dirname "$persona_asset")" "$(dirname "$portal_asset")"

cat > "$fixture/Cargo.toml" <<'EOF'
[package]
name = "embedded-asset-watch-fixture"
version = "0.0.0"
edition = "2021"
publish = false
EOF

cat > "$fixture/src/lib.rs" <<'EOF'
pub fn fixture() {}
EOF

cat > "$fixture/build.rs" <<'EOF'
include!(env!("HARN_EMBEDDED_ASSET_WATCH_MODULE"));

fn main() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    ensure_portal_fallback(manifest_dir);
    emit_watches(manifest_dir);
}
EOF

printf '%s\n' '# persona fixture' > "$persona_asset"
printf '%s\n' '// portal fixture' > "$portal_asset"

run_check() {
  local log_path=$1
  shift
  CARGO_TERM_COLOR=never \
    HARN_EMBEDDED_ASSET_WATCH_MODULE="$watch_module" \
    cargo check \
      --offline \
      --manifest-path "$fixture/Cargo.toml" \
      --target-dir "$target_dir" \
      "$@" >"$log_path" 2>&1
}

# The fallback and Vite production build must expose the same stable entry
# names. A ghost fallback file enters Cargo dep-info and then disappears when
# Vite replaces the directory, forcing an avoidable freshness recovery build.
rm -rf "$portal_root"
run_check "$tmp_root/fallback.log" --quiet
if [[ ! -f "$portal_root/index.html" \
  || ! -f "$portal_root/assets/portal/app.js" \
  || ! -f "$portal_root/assets/portal/styles.css" \
  || -e "$portal_root/assets/portal/api.js" ]]; then
  echo 'portal fallback entry points diverged from the production bundle' >&2
  find "$portal_root" -type f -print >&2 || true
  exit 1
fi

printf '%s\n' '// portal fixture' > "$portal_asset"

assert_rebuilt_for() {
  local label=$1
  local log_path=$2
  local watched_root=$3
  if ! grep -Fq 'Dirty embedded-asset-watch-fixture' "$log_path"; then
    echo "$label asset change did not dirty the fixture crate" >&2
    cat "$log_path" >&2
    exit 1
  fi
  if ! grep -Fq "$watched_root" "$log_path"; then
    echo "$label rebuild did not cite the watched root: $watched_root" >&2
    cat "$log_path" >&2
    exit 1
  fi
}

# Pin the initial fixtures to the past so each later mutation has a distinct
# Cargo fingerprint even on filesystems with coarse timestamp resolution.
touch -t 200001010101 "$persona_asset" "$portal_asset"
run_check "$tmp_root/baseline.log" --quiet

printf '\n# embedded asset rebuild test marker\n' >> "$persona_asset"
run_check "$tmp_root/persona-dirty.log" -vv
assert_rebuilt_for "persona template" "$tmp_root/persona-dirty.log" "$persona_root"

# The prior check advanced the persona fingerprint. Mutating only the portal
# now proves the second production watch root independently.
printf '\n// embedded asset rebuild test marker\n' >> "$portal_asset"
run_check "$tmp_root/portal-dirty.log" -vv
assert_rebuilt_for "portal asset" "$tmp_root/portal-dirty.log" "$portal_root"

if rg -q 'Checking harn-|Compiling harn-' \
  "$tmp_root/baseline.log" \
  "$tmp_root/persona-dirty.log" \
  "$tmp_root/portal-dirty.log"; then
  echo 'embedded asset proof compiled a Harn crate' >&2
  exit 1
fi

echo "embedded_asset_rebuild_test: ok"
