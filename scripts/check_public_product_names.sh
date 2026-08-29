#!/usr/bin/env bash
# Fail if the tracked public source tree names a specific downstream product or
# a contributor's private infrastructure (fleet hostname, home-LAN address).
# Immutable history, captured measurements, provenance, and compatibility paths
# are an explicit allowlist so the exception surface remains inspectable.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
product="burin"
brand="Burin"
# Downstream product names.
name_pattern="${product}-code|${product}-evals|${product}-commerce|${brand} Code"
# Contributor-private INFRASTRUCTURE. A product name is not the only way a
# downstream leaks into this repo: a fleet hostname or one specific machine's
# LAN address in a fixture, comment, doc example or script is just as specific,
# and reads as Harn's own. These are the values this repo has actually carried
# (harn#7592); extend the list rather than re-deriving it after the next one.
#
# Deliberately NOT a whole-subnet rule. RFC-1918 addresses are legitimate and
# common in the net-policy and SSRF fixtures, so matching 192.168.0.0/16 would
# flag a dozen correct uses and force an allowlist longer than the rule. Match
# the exact addresses that belong to a real machine instead.
host_pattern="tornadough|cattrick|${product}-mac-mini|192\.168\.86\.250"
pattern="${name_pattern}|${host_pattern}"

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
    # This file defines the patterns, so it necessarily contains them.
    scripts/check_public_product_names.sh)
      ;;
    crates/harn-hostlib/src/code_index/walker.rs|crates/harn-hostlib/tests/harn_hostlib/code_index.rs)
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
