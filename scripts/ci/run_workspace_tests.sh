#!/usr/bin/env bash
# Census the eligible suite before allowing an empty individual partition.
set -euo pipefail
partition=${1:?partition or all required}
[[ "$partition" == all || "$partition" =~ ^count:[1-3]/3$ ]] || { echo 'Invalid workspace test partition' >&2; exit 1; }
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
host_bound_filter=$(bash "$script_dir/host_bound_rust_test_filter.sh")
args=(--profile ci -E "not (${host_bound_filter})")
if [[ -n "${2:-}" ]]; then
  selected=archive
  args+=(--archive-file "$2" --workspace-remap "${GITHUB_WORKSPACE:?workspace remap required}")
else
  selected=$(bash "$script_dir/workspace_test_args.sh")
  read -ra packages <<< "$selected"
  args+=(--locked "${packages[@]}")
fi
census=$(mktemp)
trap 'rm -f "$census"' EXIT
cargo-nextest nextest list "${args[@]}" --message-format json > "$census"
if [[ -n "${RUNNER_TEMP:-}" ]]; then
  cp "$census" "$RUNNER_TEMP/harn-nextest-inventory.json"
fi
eligible=$(node "$script_dir/nextest_inventory.mjs" "$census")
echo "Workspace tests: packages=$selected eligible=$eligible partition=$partition"
if [[ "$partition" == all ]]; then
  cargo-nextest nextest run "${args[@]}" --no-tests fail
else
  cargo-nextest nextest run "${args[@]}" --partition "$partition" --no-tests pass
fi
