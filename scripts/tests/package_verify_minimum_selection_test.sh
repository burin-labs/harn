#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=scripts/lib/package_verify_dependency_resolution.sh
source "$ROOT_DIR/scripts/lib/package_verify_dependency_resolution.sh"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/harn-minimum-selection.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

cat >"$tmp/metadata.json" <<'JSON'
{
  "packages": [
    {"id":"path+vm","name":"harn-vm","version":"1.2.3"},
    {"id":"registry+rmcp","name":"rmcp","version":"3.1.4"}
  ],
  "resolve":{"nodes":[
    {"id":"path+vm","deps":[{"name":"mcp_wire","pkg":"registry+rmcp"}]},
    {"id":"registry+rmcp","deps":[]}
  ]}
}
JSON

dependency_contract_rows=($'harn-vm\t1.2.3\trmcp\t=3.1.4\t3.1.4\tmcp_wire')
selection_rows="$(select_dependency_minimums "$tmp/metadata.json" "${dependency_contract_rows[@]}")"
if [[ -n "$selection_rows" ]]; then
  echo "already-measured minimum must not request a Cargo lockfile update: $selection_rows" >&2
  exit 1
fi
emit_dependency_resolution_receipts declared-minimum "$tmp/metadata.json" 1 "${dependency_contract_rows[@]}" >"$tmp/receipt"
grep -Fq 'minimum=3.1.4 resolved=3.1.4' "$tmp/receipt"

jq '.packages[1].version = "3.1.5"' "$tmp/metadata.json" >"$tmp/higher.json"
selection_rows="$(select_dependency_minimums "$tmp/higher.json" "${dependency_contract_rows[@]}")"
[[ "$selection_rows" == $'rmcp\t3.1.4' ]]
if emit_dependency_resolution_receipts declared-minimum "$tmp/higher.json" 1 "${dependency_contract_rows[@]}" >"$tmp/higher-receipt" 2>"$tmp/higher-error"; then
  echo "minimum read-back accepted a dependency above its declared floor" >&2
  exit 1
fi
grep -Fq 'expected minimum 3.1.4' "$tmp/higher-error"

jq '.resolve.nodes[0].deps = []' "$tmp/metadata.json" >"$tmp/missing.json"
if select_dependency_minimums "$tmp/missing.json" "${dependency_contract_rows[@]}" >"$tmp/missing-selection" 2>"$tmp/missing-error"; then
  echo "missing dependency edge was mistaken for an already-satisfied floor" >&2
  exit 1
fi
grep -Fq 'expected exactly one dependency resolution edge' "$tmp/missing-error"

jq '
  .packages += [
    {"id":"path+hostlib","name":"harn-hostlib","version":"1.2.3"},
    {"id":"registry+other-rmcp","name":"rmcp","version":"3.1.5"}
  ]
  | .resolve.nodes += [
    {"id":"path+hostlib","deps":[{"name":"rmcp","pkg":"registry+other-rmcp"}]},
    {"id":"registry+other-rmcp","deps":[]}
  ]
' "$tmp/metadata.json" >"$tmp/mixed.json"
dependency_contract_rows+=($'harn-hostlib\t1.2.3\trmcp\t>=3.1.4\t3.1.4\trmcp')
selection_rows="$(select_dependency_minimums "$tmp/mixed.json" "${dependency_contract_rows[@]}")"
[[ "$selection_rows" == $'rmcp\t3.1.4' ]]
dependency_contract_rows=($'harn-vm\t1.2.3\trmcp\t=3.1.4\t3.1.4\tmcp_wire')

dependency_contract_rows+=($'harn-vm\t1.2.3\trmcp\t>=3.1.3\t3.1.3\tmcp_wire')
if select_dependency_minimums "$tmp/metadata.json" "${dependency_contract_rows[@]}" >"$tmp/conflicting-selection" 2>"$tmp/conflicting-error"; then
  echo "conflicting declared floors were accepted" >&2
  exit 1
fi
grep -Fq 'dependency contracts disagree on minimum' "$tmp/conflicting-error"

dependency_contract_rows=($'harn-vm\t1.2.3\trmcp\t<4.0.0\tnone\tmcp_wire')
[[ -z "$(select_dependency_minimums "$tmp/metadata.json" "${dependency_contract_rows[@]}")" ]]
emit_dependency_resolution_receipts declared-minimum "$tmp/metadata.json" 1 "${dependency_contract_rows[@]}" >"$tmp/ceiling-receipt"
grep -Fq 'minimum=none resolved=3.1.4' "$tmp/ceiling-receipt"
if emit_dependency_resolution_receipts declared-minimum "$tmp/missing.json" 1 "${dependency_contract_rows[@]}" >"$tmp/missing-receipt" 2>"$tmp/missing-receipt-error"; then
  echo "ceiling-only receipt accepted an unmeasured dependency edge" >&2
  exit 1
fi
grep -Fq 'expected exactly one dependency resolution edge' "$tmp/missing-receipt-error"
echo 'package minimum selection controls passed: matching floor, higher version, missing edge, mixed package edges, conflicting floors, ceiling-only receipt'
