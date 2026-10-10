#!/usr/bin/env bash
# Census the eligible suite before allowing an empty individual partition.
set -euo pipefail
partition=${1:?partition required}
[[ "$partition" =~ ^count:[1-3]/3$ ]] || { echo 'Invalid workspace test partition' >&2; exit 1; }
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
selected=$(bash "$script_dir/workspace_test_args.sh")
read -ra packages <<< "$selected"
host_bound_filter=$(bash "$script_dir/host_bound_rust_test_filter.sh")
args=(--locked "${packages[@]}" --profile ci -E "not (${host_bound_filter})")
census=$(mktemp)
trap 'rm -f "$census"' EXIT
cargo-nextest nextest list "${args[@]}" --message-format json > "$census"
eligible=$(jq -er '
  .["rust-suites"] | to_entries
  | map(.value.testcases | to_entries | map(select(.value["filter-match"].status == "matches" and .value.ignored == false)) | length)
  | add // 0
' "$census")
if (( eligible == 0 )); then
  echo 'Workspace test selection reached no eligible tests; refusing empty proof.' >&2
  exit 1
fi
echo "Workspace tests: packages=$selected eligible=$eligible partition=$partition"
cargo-nextest nextest run "${args[@]}" --partition "$partition" --no-tests pass
