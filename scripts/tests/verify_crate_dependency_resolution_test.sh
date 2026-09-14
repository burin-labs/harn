#!/usr/bin/env bash
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
filter="$root_dir/scripts/verify_crate_dependency_resolution.jq"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/harn-resolution-test.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

cat >"$tmp/metadata.json" <<'JSON'
{
  "packages": [
    {"id": "path+harn-vm", "name": "harn-vm", "version": "1.2.3"},
    {"id": "registry+rmcp", "name": "rmcp", "version": "3.1.4"}
  ],
  "resolve": {
    "nodes": [
      {
        "id": "path+harn-vm",
        "deps": [{"name": "mcp_wire", "pkg": "registry+rmcp"}]
      },
      {"id": "registry+rmcp", "deps": []}
    ]
  }
}
JSON

resolved="$(jq -er \
  --arg package harn-vm \
  --arg package_version 1.2.3 \
  --arg resolution_name mcp_wire \
  -f "$filter" \
  "$tmp/metadata.json")"
if [[ "$resolved" != "3.1.4" ]]; then
  echo "expected package-specific rmcp resolution 3.1.4, got: $resolved" >&2
  exit 1
fi

# Cargo normalizes dependency names (including renamed dependencies) to Rust
# identifiers in resolve.nodes[].deps[].name.
for dependency_name in aws-smithy-types renamed-smithy-types; do
  jq --arg dependency_name "$dependency_name" '
    .packages[1] = {"id":"registry+smithy","name":"aws-smithy-types","version":"1.6.2"}
    | .resolve.nodes[0].deps = [{"name":($dependency_name | gsub("-"; "_")),"pkg":"registry+smithy"}]
    | .resolve.nodes[1].id = "registry+smithy"
  ' "$tmp/metadata.json" >"$tmp/hyphenated.json"
  resolved="$(jq -er --arg package harn-vm --arg package_version 1.2.3 \
    --arg resolution_name "$dependency_name" -f "$filter" "$tmp/hyphenated.json")"
  if [[ "$resolved" != "1.6.2" ]]; then
    echo "hyphenated dependency did not resolve to 1.6.2: $dependency_name" >&2
    exit 1
  fi
done

jq '.packages += [{"id":"path+duplicate","name":"harn-vm","version":"1.2.3"}]' \
  "$tmp/metadata.json" >"$tmp/duplicate.json"
if jq -er \
  --arg package harn-vm \
  --arg package_version 1.2.3 \
  --arg resolution_name mcp_wire \
  -f "$filter" \
  "$tmp/duplicate.json" >"$tmp/duplicate.out" 2>"$tmp/duplicate.err"; then
  echo "duplicate packaged source nodes unexpectedly resolved" >&2
  exit 1
fi
if ! grep -Fq "expected exactly one packaged source node" "$tmp/duplicate.err"; then
  echo "duplicate-node failure did not name the violated invariant" >&2
  cat "$tmp/duplicate.err" >&2
  exit 1
fi

echo "packaged dependency resolution tests passed"
