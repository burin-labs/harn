#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

echo "=== Harn dev setup ==="

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo is required but was not found"
  exit 1
fi

git config core.hooksPath .githooks
echo "Configured git hooks path -> .githooks"

if command -v npm >/dev/null 2>&1; then
  echo "Installing repo-local Node tooling..."
  npm install

  if [[ -f tree-sitter-harn/package.json ]]; then
    echo "Installing tree-sitter-harn dependencies..."
    (cd tree-sitter-harn && npm install)
  fi

  if [[ -f editors/vscode/package.json ]]; then
    echo "Installing VS Code extension dependencies..."
    (cd editors/vscode && npm install)
  fi
else
  echo "warning: npm not found; skipping markdown, tree-sitter, and VS Code extension dependencies"
fi

if ! command -v mdbook >/dev/null 2>&1; then
  echo "warning: mdbook not found; docs builds will skip mdBook rendering"
fi

echo "Running a quick workspace build check..."
cargo check --workspace

echo ""
echo "Dev setup complete."
echo "Suggested next commands:"
echo "  make all"
echo "  make portal"
