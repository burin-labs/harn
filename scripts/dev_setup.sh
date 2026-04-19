#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

derive_target_dir() {
  local worktree_path="${HARN_DEV_TARGET_WORKTREE_PATH:-${CODEX_WORKTREE_PATH:-}}"
  if [[ -z "${worktree_path}" ]]; then
    return 1
  fi

  local worktree_leaf worktree_parent
  worktree_leaf="$(basename "${worktree_path}")"
  worktree_parent="$(basename "$(dirname "${worktree_path}")")"
  printf '%s/harn-target/%s-%s\n' "${TMPDIR:-/tmp}" "${worktree_parent}" "${worktree_leaf}"
}

write_build_config() {
  local rustc_wrapper="${1:-}"
  local target_dir="${2:-}"
  local config_path=".cargo/config.toml"
  local source_path="/dev/null"
  local tmp_path

  if [[ -z "${rustc_wrapper}" && -z "${target_dir}" ]]; then
    return 0
  fi

  mkdir -p .cargo
  if [[ -f "${config_path}" ]]; then
    source_path="${config_path}"
  fi
  tmp_path="$(mktemp)"

  awk \
    -v rustc_wrapper="${rustc_wrapper}" \
    -v target_dir="${target_dir}" \
    '
    function print_missing_build_values() {
      if (rustc_wrapper != "" && !saw_rustc_wrapper) {
        print "rustc-wrapper = \"" rustc_wrapper "\""
        saw_rustc_wrapper = 1
      }
      if (target_dir != "" && !saw_target_dir) {
        print "target-dir = \"" target_dir "\""
        saw_target_dir = 1
      }
    }

    BEGIN {
      in_build = 0
      saw_build = 0
      saw_rustc_wrapper = 0
      saw_target_dir = 0
    }

    /^\[build\][[:space:]]*$/ {
      saw_build = 1
      in_build = 1
      print
      next
    }

    /^\[[^]]+\][[:space:]]*$/ {
      if (in_build) {
        print_missing_build_values()
        in_build = 0
      }
      print
      next
    }

    {
      if (in_build && rustc_wrapper != "" && $0 ~ /^[[:space:]]*rustc-wrapper[[:space:]]*=/) {
        print "rustc-wrapper = \"" rustc_wrapper "\""
        saw_rustc_wrapper = 1
        next
      }
      if (in_build && target_dir != "" && $0 ~ /^[[:space:]]*target-dir[[:space:]]*=/) {
        print "target-dir = \"" target_dir "\""
        saw_target_dir = 1
        next
      }
      print
    }

    END {
      if (!saw_build) {
        print "[build]"
        print_missing_build_values()
      } else if (in_build) {
        print_missing_build_values()
      }
    }
    ' \
    "${source_path}" > "${tmp_path}"

  mv "${tmp_path}" "${config_path}"
}

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

target_dir="${HARN_DEV_TARGET_DIR:-}"
if [[ -z "${target_dir}" ]]; then
  target_dir="$(derive_target_dir || true)"
fi

rustc_wrapper=""
if command -v sccache >/dev/null 2>&1; then
  rustc_wrapper="sccache"
fi

write_build_config "${rustc_wrapper}" "${target_dir}"
if [[ -n "${rustc_wrapper}" ]]; then
  echo "Configured sccache as rustc wrapper in .cargo/config.toml"
fi
if [[ -n "${target_dir}" ]]; then
  mkdir -p "${target_dir}"
  echo "Configured Cargo target dir -> ${target_dir}"
fi

if command -v npm >/dev/null 2>&1; then
  echo "Installing repo-local Node tooling..."
  npm install

  if [[ -f crates/harn-cli/portal/package.json ]]; then
    ./scripts/ensure_portal_deps.sh
  fi

  if [[ -f tree-sitter-harn/package.json ]]; then
    echo "Installing tree-sitter-harn dependencies..."
    (cd tree-sitter-harn && npm install)
  fi

  if [[ -f editors/vscode/package.json ]]; then
    echo "Installing VS Code extension dependencies..."
    (cd editors/vscode && npm install)
  fi

  if [[ -f crates/harn-cli/portal/package.json ]]; then
    echo "Building portal frontend..."
    npm run portal:build
  fi
else
  echo "warning: npm not found; skipping markdown, portal, tree-sitter, and VS Code extension dependencies"
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
