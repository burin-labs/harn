#!/usr/bin/env bash
# Fail when a Rust test share would compile a third-party unit the cache writer
# does not.
#
# Cargo resolves dependency features from the packages a command selects. A
# share that selects less than the writer's whole workspace can resolve a
# different feature set for one dependency, and every unit above it then
# hashes differently: after an exact cache hit the share recompiles them all.
# Compare Cargo's own unit graphs (resolution only, nothing is compiled) for
# the writer's selection and for each share's default selection.
set -euo pipefail
export LC_ALL=C
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
partitions=(tooling rest:1/4 vm-unit:1/3 vm-integration cli-unit cli-integration)
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT

# One line per third-party unit: package, mode, target kinds, platform and
# features. A dependency resolved differently shows as its own missing line.
third_party_units() {
  RUSTC_BOOTSTRAP=1 cargo test --no-run --unit-graph -Z unstable-options "$@" > "$scratch/graph.json"
  jq -er '
    [.units[]
      | select(.pkg_id | test("^(registry|git)\\+"))
      | "\(.pkg_id) \(.mode) \(.target.kind | join(",")) \(.platform // "host") [\(.features | join(","))]"]
    | if length == 0 then error("unit graph has no third-party units") else .[] end
  ' "$scratch/graph.json" | sort -u
}

# The writer compiles the whole workspace (scripts/ci/warm_rust_cache.sh).
third_party_units --locked --workspace > "$scratch/writer"
echo "test share unit graphs: writer third_party_units=$(wc -l < "$scratch/writer" | tr -d ' ')"
diverged=0
for partition in "${partitions[@]}"; do
  selection=()
  while IFS= read -r arg; do selection+=("$arg"); done < <(
    POST_MERGE_TIER_ACTIVE=false bash "$script_dir/run_workspace_tests.sh" --print-selection "$partition")
  ((${#selection[@]} > 0)) || { echo "test share unit graphs: $partition printed no selection" >&2; exit 1; }
  third_party_units "${selection[@]}" > "$scratch/share"
  missing=$(comm -13 "$scratch/writer" "$scratch/share" | wc -l | tr -d ' ')
  echo "test share unit graphs: partition=$partition third_party_units=$(wc -l < "$scratch/share" | tr -d ' ') missing_from_writer=$missing"
  if ((missing > 0)); then
    diverged=1
    echo "::error::Rust test share $partition compiles third-party units the cache writer does not; it would recompile them after an exact cache hit. First ten:" >&2
    comm -13 "$scratch/writer" "$scratch/share" | head -n 10 >&2
  fi
done
exit "$diverged"
