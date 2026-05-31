#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

mdbook build "$repo_root/docs"

while IFS= read -r -d '' src; do
  rel="${src#"$repo_root/docs/src/"}"
  dest="$repo_root/docs/dist/$rel"
  mkdir -p "$(dirname "$dest")"
  cp "$src" "$dest"
done < <(find "$repo_root/docs/src" -type f -name '*.md' -print0)

rm -f "$repo_root/docs/dist/SUMMARY.md"

mkdir -p "$repo_root/docs/dist/docs/llm"
cp "$repo_root/docs/llm/harn-quickref.md" \
  "$repo_root/docs/dist/docs/llm/harn-quickref.md"
cp "$repo_root/docs/llm/harn-triggers-quickref.md" \
  "$repo_root/docs/dist/docs/llm/harn-triggers-quickref.md"
