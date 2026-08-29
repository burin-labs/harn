#!/usr/bin/env bash
# Fail if the tracked public source tree names a specific downstream product.
# Immutable history, captured measurements, provenance, and compatibility paths
# are an explicit allowlist so the exception surface remains inspectable.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
product="burin"
brand="Burin"
pattern="${product}-code|${product}-evals|${product}-commerce|${brand} Code"

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/harn-public-product-names.XXXXXX")"
trap 'rm -rf "$tmp_dir"' EXIT

set +e
git -C "$repo_root" grep -n -I -E -- "$pattern" >"$tmp_dir/all-hits.txt"
scan_status=$?
set -e
if [[ "$scan_status" -gt 1 ]]; then
  echo "error: failed to scan tracked source files for downstream product names" >&2
  exit "$scan_status"
fi

while IFS= read -r hit; do
  path="${hit%%:*}"
  case "$path" in
    CHANGELOG.md|changelog/archive/*|experiments/step-judge/results/*)
      ;;
    spec/provider-catalog/provider-catalog.json)
      ;;
    crates/harn-vm/src/llm/catalog_sources/50-presentation/00-model-selection.toml)
      ;;
    crates/harn-vm/src/llm/providers.toml)
      ;;
    scripts/agent_shell_guard_policy.harn|scripts/tests/agent_shell_guard_test.harn)
      ;;
    crates/harn-hostlib/src/code_index/walker.rs|crates/harn-hostlib/tests/harn_hostlib/code_index.rs)
      ;;
    docs/rfcs/tool-calling-north-star.md)
      ;;
    *)
      printf '%s\n' "$hit" >>"$tmp_dir/hits.txt"
      ;;
  esac
done <"$tmp_dir/all-hits.txt"

if [[ -s "$tmp_dir/hits.txt" ]]; then
  echo "error: tracked source files name a specific downstream host product:" >&2
  cat "$tmp_dir/hits.txt" >&2
  echo >&2
  echo "Use host-neutral wording (downstream host, host repo, packager)." >&2
  exit 1
fi

echo "public source product-name scan passed"
