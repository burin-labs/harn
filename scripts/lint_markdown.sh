#!/usr/bin/env bash
set -euo pipefail

# Staged tooling also runs with published CLIs that cannot know newer runtime keys.
# The tool-owned extension namespace keeps those CLIs usable during reaudits.
policy_root="${HARN_EXT_RELEASE_MARKDOWNLINT_CONFIG_ROOT:-.}"
policy=(--config "$policy_root/.markdownlint.jsonc" --ignore-path "$policy_root/.markdownlintignore")
documents=()
fragments=()
if [[ $# -eq 0 ]]; then
  documents=("**/*.md")
  fragments=("changelog.d/*.md")
else
  for path in "$@"; do
    case "$path" in
      changelog.d/*.md) fragments+=("$path") ;;
      *) documents+=("$path") ;;
    esac
  done
fi

if [[ ${#documents[@]} -gt 0 ]]; then
  npx --no-install markdownlint "${policy[@]}" --ignore "changelog.d/*.md" -- "${documents[@]}"
fi
# Release fragments are snippets rather than standalone documents.
if [[ ${#fragments[@]} -gt 0 ]]; then
  npx --no-install markdownlint "${policy[@]}" --disable MD041 MD025 -- "${fragments[@]}"
fi
