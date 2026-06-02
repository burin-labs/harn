#!/usr/bin/env bash
#
# Build the harnlang.com site into docs/dist.
#
# The site is a Vite + React + Tailwind app under website/ that renders the
# Diataxis-structured Markdown in docs/src (kept in place so every generator and
# checker — check-docs-snippets, the language-spec mirror, diagnostics,
# harn-keywords.js — is unaffected). `npm run build` produces a statically
# prerendered, fully crawlable site in docs/dist.
#
# This is the canonical docs build entry point: release_gate.sh runs it, and
# Render's static-site build command points at it (publish dir: docs/dist).
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
site_dir="$repo_root/website"
dist_dir="$repo_root/docs/dist"

if ! command -v npm >/dev/null 2>&1; then
  echo "error: npm (Node.js) is required to build the docs site." >&2
  echo "       install Node 20+ and re-run, or use the Render build command." >&2
  exit 1
fi

# Deterministic install when a lockfile is present; fall back to `npm install`
# for environments without one.
if [[ -f "$site_dir/package-lock.json" ]]; then
  (cd "$site_dir" && npm ci)
else
  (cd "$site_dir" && npm install)
fi

# tsc + vite client build + vite ssr build + prerender → docs/dist.
(cd "$site_dir" && npm run build)

# Mirror the raw Markdown sources next to the rendered HTML so agents and tools
# can fetch the canonical .md for any page at the same path (preserved contract
# from the mdBook build).
while IFS= read -r -d '' src; do
  rel="${src#"$repo_root/docs/src/"}"
  dest="$dist_dir/$rel"
  mkdir -p "$(dirname "$dest")"
  cp "$src" "$dest"
done < <(find "$repo_root/docs/src" -type f -name '*.md' -print0)

# SUMMARY.md is the nav definition, not a page — drop it from the output.
rm -f "$dist_dir/SUMMARY.md"

# LLM quick-reference files agents fetch directly (generated artifacts).
mkdir -p "$dist_dir/docs/llm"
cp "$repo_root/docs/llm/harn-quickref.md" \
  "$dist_dir/docs/llm/harn-quickref.md"
cp "$repo_root/docs/llm/harn-triggers-quickref.md" \
  "$dist_dir/docs/llm/harn-triggers-quickref.md"

echo "docs site built → $dist_dir"
