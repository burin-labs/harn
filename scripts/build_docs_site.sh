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

# Every repository path this build reads, and therefore every path whose change
# must republish the site. The CI docs-deploy gate consumes this list via
# `--print-inputs` so the published input set has exactly one owner and cannot
# drift from what the build actually reads below.
#
# It previously restated the set by hand as `docs/|website/|build_docs_site.sh`,
# which was wrong in both directions: it fired on unpublished subtrees
# (docs/perf, docs/rfcs, docs/design), and — the reason this matters — it missed
# artifacts the build copies into the published output. Refreshing
# spec/provider-catalog/provider-catalog.json touched nothing else in the
# pattern, so harnlang.com/provider-catalog/provider-catalog.json (the CLI's
# default catalog refresh URL) kept serving a stale catalog until an unrelated
# docs commit happened to redeploy. install.sh and install.ps1 back the
# documented `curl … | sh` one-liners and had the same latent gap.
#
# Keep this list in sync by construction: if you add a `cp` from a new path
# below, add that path here.
docs_site_inputs=(
  website
  docs/src
  docs/theme
  docs/llm/harn-quickref.md
  docs/llm/harn-triggers-quickref.md
  install.sh
  install.ps1
  spec/provider-catalog/provider-catalog.json
  scripts/build_docs_site.sh
)

# Answer before the npm probe: the gate only needs the path list, and runs on a
# runner that has not installed Node.
if [[ "${1:-}" == "--print-inputs" ]]; then
  printf '%s\n' "${docs_site_inputs[@]}"
  exit 0
fi

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

# Installer scripts served from the site root so the documented one-liners
# (`curl -fsSL https://harnlang.com/install.sh | sh` and
# `irm https://harnlang.com/install.ps1 | iex`) resolve.
cp "$repo_root/install.sh" "$dist_dir/install.sh"
cp "$repo_root/install.ps1" "$dist_dir/install.ps1"

# Provider/model catalog served from the site so the CLI's default refresh URL
# (https://harnlang.com/provider-catalog/provider-catalog.json) resolves.
mkdir -p "$dist_dir/provider-catalog"
cp "$repo_root/spec/provider-catalog/provider-catalog.json" \
  "$dist_dir/provider-catalog/provider-catalog.json"

echo "docs site built → $dist_dir"
