#!/usr/bin/env bash
set -euo pipefail

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
  npx --no-install markdownlint --ignore "changelog.d/*.md" -- "${documents[@]}"
fi
# Release fragments are snippets rather than standalone documents.
if [[ ${#fragments[@]} -gt 0 ]]; then
  npx --no-install markdownlint --disable MD041 MD025 -- "${fragments[@]}"
fi
