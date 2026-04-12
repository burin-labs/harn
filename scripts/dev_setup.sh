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

# Install optional but recommended Cargo tools.
for tool_spec in "cargo-nextest:cargo-nextest --locked" "sccache:sccache --locked"; do
  tool="${tool_spec%%:*}"
  install_args="${tool_spec#*:}"
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "Installing $tool..."
    cargo install $install_args || echo "warning: failed to install $tool (non-fatal)"
  else
    echo "$tool already installed."
  fi
done

# Enable sccache as the rustc wrapper if installed. The generated config is
# gitignored so fresh clones without sccache still work.
if command -v sccache >/dev/null 2>&1; then
  mkdir -p .cargo
  if [[ ! -f .cargo/config.toml ]] || ! grep -q rustc-wrapper .cargo/config.toml 2>/dev/null; then
    cat >> .cargo/config.toml <<'TOML'
[build]
rustc-wrapper = "sccache"
TOML
    echo "Configured sccache as rustc wrapper in .cargo/config.toml"
  fi
fi

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
