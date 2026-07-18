#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

SETUP_STATE_DIR="${HARN_DEV_SETUP_STATE_DIR:-$ROOT_DIR/.codex/dev-setup}"
SETUP_PROFILE="${HARN_DEV_SETUP_PROFILE:-full}"
MANAGED_CARGO_CONFIG_MARKER="harn-dev-setup-managed"

log() {
  printf '\n==> %s\n' "$*"
}

derive_target_dir() {
  local worktree_path="${HARN_DEV_TARGET_WORKTREE_PATH:-${CODEX_WORKTREE_PATH:-}}"
  if [[ -z "${worktree_path}" ]]; then
    return 1
  fi

  local worktree_leaf worktree_parent
  worktree_leaf="$(basename "${worktree_path}")"
  worktree_parent="$(basename "$(dirname "${worktree_path}")")"
  printf '%s/harn-target/%s-%s\n' "${HARN_DEV_SETUP_STORAGE_ROOT}" "${worktree_parent}" "${worktree_leaf}"
}

derive_storage_root() {
  if [[ -n "${HARN_DEV_SETUP_STORAGE_ROOT:-}" ]]; then
    printf '%s\n' "${HARN_DEV_SETUP_STORAGE_ROOT}"
  elif [[ "${SETUP_PROFILE}" == "rust" ]]; then
    printf '%s/harn/dev-setup\n' "${XDG_CACHE_HOME:-$HOME/.cache}"
  else
    printf '%s\n' "${TMPDIR:-/tmp}"
  fi
}

derive_tool_target_dir() {
  if [[ -n "${HARN_DEV_SETUP_TOOL_TARGET_DIR:-}" ]]; then
    printf '%s\n' "${HARN_DEV_SETUP_TOOL_TARGET_DIR}"
    return
  fi

  printf '%s/harn/cargo-install\n' "${XDG_CACHE_HOME:-$HOME/.cache}"
}

write_build_config() {
  local rustc_wrapper="${1:-}"
  local target_dir="${2:-}"
  local build_dir="${3:-}"
  local config_path=".cargo/config.toml"
  local drop_generated_target_dir=0
  local drop_generated_build_dir=0
  local source_path="/dev/null"
  local tmp_path

  if [[ -z "${target_dir}" ]]; then
    drop_generated_target_dir=1
  fi

  if [[ -z "${build_dir}" ]]; then
    drop_generated_build_dir=1
  fi

  if [[ -z "${rustc_wrapper}" && -z "${target_dir}" && -z "${build_dir}" && ! -f "${config_path}" ]]; then
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
    -v build_dir="${build_dir}" \
    -v managed_marker="${MANAGED_CARGO_CONFIG_MARKER}" \
    -v drop_generated_target_dir="${drop_generated_target_dir}" \
    -v drop_generated_build_dir="${drop_generated_build_dir}" \
    '
    function extract_toml_string(line, value) {
      value = line
      sub(/^[^=]*=[[:space:]]*"/, "", value)
      sub(/"[[:space:]]*(#.*)?$/, "", value)
      return value
    }

    function is_generated_target_dir(value) {
      return value ~ "^(/private)?/tmp/harn-devsetup-[^/]+$" || \
        value ~ "^(/private)?/tmp/harn-target/[^/]+$" || \
        value ~ "/T/+harn-target/[^/]+$"
    }

    function is_generated_build_dir(value) {
      return value ~ "^(/private)?/tmp/cargo-build-shared$" || \
        value ~ "/T/+cargo-build-shared$"
    }

    function is_managed_line(line) {
      return line ~ "#[[:space:]]*" managed_marker "[[:space:]]*$"
    }

    function managed_value_line(key, value) {
      return key " = \"" value "\" # " managed_marker
    }

    function print_missing_build_values() {
      if (rustc_wrapper != "" && !saw_rustc_wrapper) {
        print "rustc-wrapper = \"" rustc_wrapper "\""
        saw_rustc_wrapper = 1
      }
      if (target_dir != "" && !saw_target_dir) {
        print managed_value_line("target-dir", target_dir)
        saw_target_dir = 1
      }
      if (build_dir != "" && !saw_build_dir) {
        print managed_value_line("build-dir", build_dir)
        saw_build_dir = 1
      }
    }

    BEGIN {
      in_build = 0
      saw_build = 0
      saw_rustc_wrapper = 0
      saw_target_dir = 0
      saw_build_dir = 0
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
        print managed_value_line("target-dir", target_dir)
        saw_target_dir = 1
        next
      }
      if (in_build && drop_generated_target_dir && $0 ~ /^[[:space:]]*target-dir[[:space:]]*=/) {
        if (is_managed_line($0) || is_generated_target_dir(extract_toml_string($0))) {
          saw_target_dir = 1
          next
        }
      }
      if (in_build && build_dir != "" && $0 ~ /^[[:space:]]*build-dir[[:space:]]*=/) {
        print managed_value_line("build-dir", build_dir)
        saw_build_dir = 1
        next
      }
      if (in_build && drop_generated_build_dir && $0 ~ /^[[:space:]]*build-dir[[:space:]]*=/) {
        if (is_managed_line($0) || is_generated_build_dir(extract_toml_string($0))) {
          saw_build_dir = 1
          next
        }
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

hash_setup_inputs() {
  local name="$1"
  shift

  {
    printf '%s\n' "${name}"
    for path in "$@"; do
      if [[ -f "${path}" ]]; then
        shasum -a 256 "${path}"
      elif [[ -d "${path}" ]]; then
        find "${path}" \
          \( -name node_modules -o -name target -o -name .git -o -name dist -o -name portal-dist \) -prune \
          -o -type f -print0 \
          | sort -z \
          | xargs -0 shasum -a 256
      fi
    done
    true
  } | shasum -a 256 | awk '{print $1}'
}

cargo_setup_fingerprint() {
  {
    printf 'cargo-check:v1\n'
    find . \
      \( -name target -o -name '.target-*' -o -name .codex -o -name .claude -o -name node_modules -o -name .git \) -prune \
      -o \( -name Cargo.toml -o -name Cargo.lock -o -name build.rs \) -type f -print0 \
      | sort -z \
      | xargs -0 shasum -a 256
    true
  } | shasum -a 256 | awk '{print $1}'
}

run_setup_step() {
  local label="$1"
  local stamp="$2"
  shift 2

  local required=()
  while [[ "$#" -gt 0 && "$1" != "--" ]]; do
    required+=("$1")
    shift
  done
  shift

  if [[ "${HARN_DEV_SETUP_FORCE:-0}" != "1" && -f "${stamp}" ]]; then
    local ready=1
    local required_path
    for required_path in "${required[@]}"; do
      if [[ ! -e "${required_path}" ]]; then
        ready=0
        break
      fi
    done

    if [[ "${ready}" -eq 1 ]]; then
      echo "${label} up to date."
      return 0
    fi
  fi

  log "${label}"
  "$@"
  touch "${stamp}"
}

echo "=== Harn dev setup ==="

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo is required but was not found"
  exit 1
fi

case "${SETUP_PROFILE}" in
  full | rust)
    ;;
  *)
    echo "error: HARN_DEV_SETUP_PROFILE must be 'full' or 'rust' (got '${SETUP_PROFILE}')" >&2
    exit 1
    ;;
esac

echo "Setup profile -> ${SETUP_PROFILE}"
HARN_DEV_SETUP_STORAGE_ROOT="$(derive_storage_root)"
export HARN_DEV_SETUP_STORAGE_ROOT
echo "Setup storage root -> ${HARN_DEV_SETUP_STORAGE_ROOT}"

git config core.hooksPath .githooks
echo "Configured git hooks path -> .githooks"
./scripts/configure_merge_drivers.sh

target_dir="${HARN_DEV_TARGET_DIR:-}"
if [[ -z "${target_dir}" ]]; then
  target_dir="$(derive_target_dir || true)"
fi

build_dir="${HARN_DEV_BUILD_DIR:-}"

rustc_wrapper=""
if command -v sccache >/dev/null 2>&1; then
  rustc_wrapper="sccache"
fi

write_build_config "${rustc_wrapper}" "${target_dir}" "${build_dir}"
if [[ -n "${rustc_wrapper}" ]]; then
  echo "Configured sccache as rustc wrapper in .cargo/config.toml"
fi
if [[ -n "${target_dir}" ]]; then
  mkdir -p "${target_dir}"
  echo "Configured Cargo target dir -> ${target_dir}"
fi
if [[ -n "${build_dir}" ]]; then
  mkdir -p "${build_dir}"
  echo "Configured custom Cargo build dir -> ${build_dir}"
fi

if [[ "${SETUP_PROFILE}" == "full" ]]; then
  tool_target_dir="$(derive_tool_target_dir)"
  mkdir -p "${tool_target_dir}"

  # Keep optional tool compilation off quota-limited transient directories.
  # The durable target also makes repeated setup attempts reuse intermediates.
  for tool_spec in "cargo-nextest:cargo-nextest --locked" "sccache:sccache --locked"; do
    tool="${tool_spec%%:*}"
    read -r -a install_args <<< "${tool_spec#*:}"
    if ! command -v "$tool" >/dev/null 2>&1; then
      echo "Installing $tool (build artifacts: ${tool_target_dir})..."
      if ! cargo install --target-dir "${tool_target_dir}" "${install_args[@]}"; then
        echo "warning: failed to install $tool (non-fatal)"
      fi
    else
      echo "$tool already installed."
    fi
  done

  if ! command -v actionlint >/dev/null 2>&1; then
    if command -v go >/dev/null 2>&1; then
      echo "Installing actionlint..."
      go install github.com/rhysd/actionlint/cmd/actionlint@v1.7.12 || echo "warning: failed to install actionlint (non-fatal)"
    else
      echo "warning: go not found; skipping actionlint install"
    fi
  else
    echo "actionlint already installed."
  fi
fi

mkdir -p "${SETUP_STATE_DIR}"

# Reclaim per-worktree target dirs left behind by worktrees that have since
# been removed. Best-effort; never fail setup over housekeeping.
if [[ -x ./scripts/prune_stale_targets.sh ]]; then
  prune_stamp="${SETUP_STATE_DIR}/prune-stale-targets.stamp"
  prune_interval="${HARN_DEV_SETUP_PRUNE_SECONDS:-86400}"
  should_prune=1

  if [[ "${HARN_DEV_SETUP_FORCE:-0}" != "1" && -f "${prune_stamp}" ]]; then
    last_prune="$(stat -f %m "${prune_stamp}" 2>/dev/null || stat -c %Y "${prune_stamp}" 2>/dev/null || echo 0)"
    now="$(date +%s)"
    if (( now - last_prune < prune_interval )); then
      should_prune=0
    fi
  fi

  if [[ "${should_prune}" -eq 1 ]]; then
    ./scripts/prune_stale_targets.sh && touch "${prune_stamp}" || true
  else
    echo "harn-target GC recently checked."
  fi
fi

if [[ "${SETUP_PROFILE}" == "full" ]] && command -v npm >/dev/null 2>&1; then
  root_node_fp="$(hash_setup_inputs root-node package.json package-lock.json)"
  run_setup_step \
    "Installing repo-local Node tooling" \
    "${SETUP_STATE_DIR}/root-node-${root_node_fp}.stamp" \
    node_modules \
    -- npm install --no-audit --fund=false

  if [[ -f crates/harn-cli/portal/package.json ]]; then
    portal_deps_fp="$(hash_setup_inputs portal-deps scripts/ensure_portal_deps.sh crates/harn-cli/portal/package.json crates/harn-cli/portal/package-lock.json)"
    run_setup_step \
      "Installing portal frontend dependencies" \
      "${SETUP_STATE_DIR}/portal-deps-${portal_deps_fp}.stamp" \
      crates/harn-cli/portal/node_modules \
      -- ./scripts/ensure_portal_deps.sh
  fi

  if [[ -f tree-sitter-harn/package.json ]]; then
    tree_sitter_fp="$(hash_setup_inputs tree-sitter-node tree-sitter-harn/package.json tree-sitter-harn/package-lock.json)"
    run_setup_step \
      "Installing tree-sitter-harn dependencies" \
      "${SETUP_STATE_DIR}/tree-sitter-node-${tree_sitter_fp}.stamp" \
      tree-sitter-harn/node_modules \
      -- bash -c 'cd tree-sitter-harn && npm install --no-audit --fund=false'
  fi

  if [[ -f editors/vscode/package.json ]]; then
    vscode_fp="$(hash_setup_inputs vscode-node editors/vscode/package.json editors/vscode/package-lock.json)"
    run_setup_step \
      "Installing VS Code extension dependencies" \
      "${SETUP_STATE_DIR}/vscode-node-${vscode_fp}.stamp" \
      editors/vscode/node_modules \
      -- bash -c 'cd editors/vscode && npm install --no-audit --fund=false'
  fi

  if [[ -f crates/harn-cli/portal/package.json ]]; then
    portal_build_fp="$(hash_setup_inputs portal-build package.json package-lock.json scripts/ensure_portal_deps.sh crates/harn-cli/portal)"
    run_setup_step \
      "Building portal frontend" \
      "${SETUP_STATE_DIR}/portal-build-${portal_build_fp}.stamp" \
      crates/harn-cli/portal-dist/index.html \
      -- npm run portal:build
  fi

  if [[ -f website/package.json ]]; then
    website_fp="$(hash_setup_inputs website-node website/package.json website/package-lock.json)"
    run_setup_step \
      "Installing harnlang.com site dependencies" \
      "${SETUP_STATE_DIR}/website-node-${website_fp}.stamp" \
      website/node_modules \
      -- bash -c 'cd website && npm install --no-audit --fund=false'
  fi
elif [[ "${SETUP_PROFILE}" == "full" ]]; then
  echo "warning: npm not found; skipping markdown, portal, tree-sitter, VS Code extension, and docs-site dependencies"
fi

cargo_target_root="${target_dir:-$ROOT_DIR/target}"
cargo_fp="$(cargo_setup_fingerprint)"
run_setup_step \
  "Running a quick workspace build check" \
  "${SETUP_STATE_DIR}/cargo-check-${cargo_fp}.stamp" \
  "${cargo_target_root}/debug" \
  -- cargo check --workspace

# macOS-only: sign any locally-built harn binaries with the team Developer
# ID Application identity so Gatekeeper doesn't pop up "Verifying harn..."
# when agents in fresh worktrees launch them. No-op on non-macOS or when
# the cert isn't in the user's login keychain (the script self-skips with
# a hint pointing at the team .p12 in 1Password).
./scripts/sign_local_macos.sh

echo ""
echo "Dev setup complete."
echo "Suggested next commands:"
echo "  make all"
echo "  make portal"
