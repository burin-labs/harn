#!/usr/bin/env bash
# Fail if public contract files name a specific downstream host product.
# CHANGELOG history, tests, and the protocol-artifact sibling-repo check are
# out of scope here; this gate owns the files a stranger reads first.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
pattern='burin-code|burin-evals|burin-commerce|Burin Code'

scan_paths=(
  ".github/SECURITY.md"
  ".github/workflows"
  ".github/actions"
  ".gitignore"
)

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/harn-public-product-names.XXXXXX")"
trap 'rm -rf "$tmp_dir"' EXIT

set +e
(
  cd "$repo_root"
  grep -R -n -E -- "$pattern" "${scan_paths[@]}"
) >"$tmp_dir/hits.txt"
scan_status=$?
set -e
if [[ "$scan_status" -gt 1 ]]; then
  echo "error: failed to scan public contract files for downstream product names" >&2
  exit "$scan_status"
fi

if [[ -s "$tmp_dir/hits.txt" ]]; then
  echo "error: public contract files name a specific downstream host product:" >&2
  cat "$tmp_dir/hits.txt" >&2
  echo >&2
  echo "Use host-neutral wording (downstream host, host repo, packager)." >&2
  exit 1
fi

echo "public contract product-name scan passed"
