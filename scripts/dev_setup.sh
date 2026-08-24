#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"
# shellcheck source=lib/file_time.sh
source "$ROOT_DIR/scripts/lib/file_time.sh"

SETUP_STATE_DIR="${HARN_DEV_SETUP_STATE_DIR:-$ROOT_DIR/.codex/dev-setup}"
SETUP_PROFILE="${HARN_DEV_SETUP_PROFILE:-full}"
MANAGED_CARGO_CONFIG_MARKER="harn-dev-setup-managed"

log() {
  printf '\n==> %s\n' "$*"
}

derive_target_dir() {
  local worktree_var="HARN_DEV_TARGET_WORKTREE_PATH"
  local worktree_path="${HARN_DEV_TARGET_WORKTREE_PATH:-}"
  if [[ -z "${worktree_path}" ]]; then
    worktree_var="CODEX_WORKTREE_PATH"
    worktree_path="${CODEX_WORKTREE_PATH:-}"
  fi
  if [[ -z "${worktree_path}" ]]; then
    worktree_var="current checkout"
    worktree_path="${ROOT_DIR}"
  fi

  # The configured path names the worktree this target dir belongs to. When it
  # points somewhere other than the checkout being set up, honouring it hands
  # two checkouts the same target dir, and generated OUT_DIR contents are
  # mutable -- the concurrent-worktree hazard AGENTS.md forbids. This is easy to
  # hit without noticing: an agent session that exports its primary checkout
  # path keeps exporting it while working in a sibling worktree, and a value
  # inherited from a parent shell outlives the worktree it was set for. Derive
  # from the checkout actually being set up instead, and say so.
  local configured_root actual_root
  actual_root="$(cd "${ROOT_DIR}" && pwd -P)"
  configured_root="$(cd "${worktree_path}" 2>/dev/null && pwd -P)" || configured_root=""
  if [[ "${configured_root}" != "${actual_root}" ]]; then
    printf 'warning: %s=%s does not name this checkout (%s); deriving the target dir from this checkout so concurrent worktrees do not share one\n' \
      "${worktree_var}" "${worktree_path}" "${actual_root}" >&2
    worktree_path="${actual_root}"
  fi

  local worktree_leaf worktree_parent
  worktree_leaf="$(basename "${worktree_path}")"
  worktree_parent="$(basename "$(dirname "${worktree_path}")")"
  printf '%s/harn-target/%s-%s\n' "${HARN_DEV_SETUP_STORAGE_ROOT}" "${worktree_parent}" "${worktree_leaf}"
}

derive_storage_root() {
  if [[ -n "${HARN_DEV_SETUP_STORAGE_ROOT:-}" ]]; then
    printf '%s\n' "${HARN_DEV_SETUP_STORAGE_ROOT}"
  else
    # Every setup profile is a different amount of work over the same
    # worktree-owned build state. Keeping one durable root means the expected
    # bootstrap -> full transition reuses its Cargo target instead of deleting
    # the managed setting and paying a second cold workspace build.
    printf '%s/harn/dev-setup\n' "${XDG_CACHE_HOME:-$HOME/.cache}"
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
  local force_target_dir="${4:-0}"
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
    -v force_target_dir="${force_target_dir}" \
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
        if (force_target_dir || is_managed_line($0) || is_generated_target_dir(extract_toml_string($0))) {
          print managed_value_line("target-dir", target_dir)
        } else {
          # A user-owned Cargo target remains authoritative unless the caller
          # explicitly supplied HARN_DEV_TARGET_DIR.
          print
        }
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

configured_cargo_target_dir() {
  awk '
    /^\[build\][[:space:]]*$/ { in_build = 1; next }
    /^\[[^]]+\][[:space:]]*$/ { in_build = 0 }
    in_build && /^[[:space:]]*target-dir[[:space:]]*=/ {
      value = $0
      sub(/^[^=]*=[[:space:]]*"/, "", value)
      sub(/"[[:space:]]*(#.*)?$/, "", value)
      print value
      exit
    }
  ' .cargo/config.toml
}

write_sccache_env_config() {
  local enabled="$1"
  local config_path=".cargo/config.toml"
  local tmp_path

  [[ -f "${config_path}" ]] || return 0
  tmp_path="$(mktemp)"

  # Drop only our generated value. A user-owned SCCACHE_BASEDIRS remains
  # authoritative and suppresses insertion below.
  awk -v managed_marker="${MANAGED_CARGO_CONFIG_MARKER}" '
    $0 ~ /^[[:space:]]*SCCACHE_BASEDIRS[[:space:]]*=/ &&
      $0 ~ "#[[:space:]]*" managed_marker "[[:space:]]*$" { next }
    { print }
  ' "${config_path}" > "${tmp_path}"
  mv "${tmp_path}" "${config_path}"

  if [[ "${enabled}" != "1" ]] \
    || grep -Eq '^[[:space:]]*SCCACHE_BASEDIRS[[:space:]]*=' "${config_path}"; then
    return 0
  fi

  local managed_line
  managed_line='SCCACHE_BASEDIRS = { value = ".", relative = true, force = true } # harn-dev-setup-managed'
  if grep -Eq '^\[env\][[:space:]]*$' "${config_path}"; then
    tmp_path="$(mktemp)"
    awk -v managed_line="${managed_line}" '
      { print }
      /^\[env\][[:space:]]*$/ { print managed_line }
    ' "${config_path}" > "${tmp_path}"
    mv "${tmp_path}" "${config_path}"
  else
    printf '\n[env]\n%s\n' "${managed_line}" >> "${config_path}"
  fi
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

setup_requirements_ready() {
  local required_path
  for required_path in "$@"; do
    if [[ "${required_path}" == executable:* ]]; then
      [[ -x "${required_path#executable:}" ]] || return 1
    else
      [[ -e "${required_path}" ]] || return 1
    fi
  done
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
    if setup_requirements_ready "${required[@]}"; then
      echo "${label} up to date."
      return 0
    fi
  fi

  log "${label}"
  "$@"
  if ! setup_requirements_ready "${required[@]}"; then
    echo "error: ${label} completed without producing its required artifact" >&2
    return 1
  fi
  touch "${stamp}"
}

install_locked_node_dependencies() {
  local directory="$1"

  (
    cd "${directory}"
    npm ci --no-audit --fund=false
  )
}

echo "=== Harn dev setup ==="

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo is required but was not found"
  exit 1
fi

case "${SETUP_PROFILE}" in
  full | rust | bootstrap)
    ;;
  *)
    echo "error: HARN_DEV_SETUP_PROFILE must be 'full', 'rust', or 'bootstrap' (got '${SETUP_PROFILE}')" >&2
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

explicit_target_dir="${HARN_DEV_TARGET_DIR:-}"
target_dir="${explicit_target_dir}"
if [[ -z "${target_dir}" ]]; then
  target_dir="$(derive_target_dir)"
fi

build_dir="${HARN_DEV_BUILD_DIR:-}"

rustc_wrapper=""
if command -v sccache >/dev/null 2>&1; then
  rustc_wrapper="sccache"
fi

force_target_dir=0
[[ -n "${explicit_target_dir}" ]] && force_target_dir=1
write_build_config "${rustc_wrapper}" "${target_dir}" "${build_dir}" "${force_target_dir}"
if [[ -n "${rustc_wrapper}" ]]; then
  write_sccache_env_config 1
else
  write_sccache_env_config 0
fi
if [[ -n "${rustc_wrapper}" ]]; then
  echo "Configured sccache as rustc wrapper in .cargo/config.toml"
fi
if [[ -n "${target_dir}" ]] && grep -Fxq \
  "target-dir = \"${target_dir}\" # ${MANAGED_CARGO_CONFIG_MARKER}" \
  .cargo/config.toml; then
  mkdir -p "${target_dir}"
  echo "Configured Cargo target dir -> ${target_dir}"
  if [[ -x ./scripts/cargo_target_seed.sh ]]; then
    ./scripts/cargo_target_seed.sh restore "${target_dir}" "${HARN_DEV_SETUP_STORAGE_ROOT}"
  fi
elif [[ -n "${target_dir}" ]]; then
  echo "Preserved user-owned Cargo target dir from .cargo/config.toml"
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
#
# The state dir this is stamped against is per-worktree, so every new worktree
# would otherwise pay a full sweep of the shared target root -- measured at
# ~40s across 12 roots. Bootstrap exists to configure a worktree for its first
# Cargo probe and to be waited on interactively, so housekeeping stays out of
# it; the profiles that already compile absorb the sweep instead.
if [[ "${SETUP_PROFILE}" != "bootstrap" ]] && [[ -x ./scripts/prune_stale_targets.sh ]]; then
  prune_stamp="${SETUP_STATE_DIR}/prune-stale-targets.stamp"
  prune_interval="${HARN_DEV_SETUP_PRUNE_SECONDS:-86400}"
  should_prune=1

  if [[ "${HARN_DEV_SETUP_FORCE:-0}" != "1" && -f "${prune_stamp}" ]]; then
    last_prune="$(file_mtime_epoch "${prune_stamp}" || printf '0\n')"
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

if [[ "${SETUP_PROFILE}" == "full" && -f package.json ]] \
  && command -v npm >/dev/null 2>&1; then
  root_node_fp="$(hash_setup_inputs root-node package.json package-lock.json)"
  run_setup_step \
    "Installing repo-local Node tooling" \
    "${SETUP_STATE_DIR}/root-node-${root_node_fp}.stamp" \
    node_modules \
    -- install_locked_node_dependencies .

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
      -- install_locked_node_dependencies tree-sitter-harn
  fi

  if [[ -f editors/vscode/package.json ]]; then
    vscode_fp="$(hash_setup_inputs vscode-node editors/vscode/package.json editors/vscode/package-lock.json)"
    run_setup_step \
      "Installing VS Code extension dependencies" \
      "${SETUP_STATE_DIR}/vscode-node-${vscode_fp}.stamp" \
      editors/vscode/node_modules \
      -- install_locked_node_dependencies editors/vscode
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
      -- install_locked_node_dependencies website
  fi
elif [[ "${SETUP_PROFILE}" == "full" ]]; then
  echo "warning: npm not found; skipping markdown, portal, tree-sitter, VS Code extension, and docs-site dependencies"
fi

if [[ "${SETUP_PROFILE}" != "bootstrap" ]]; then
  cargo_target_root="$(configured_cargo_target_dir)"
  cargo_target_root="${CARGO_TARGET_DIR:-${cargo_target_root:-$ROOT_DIR/target}}"
  # The resolver owns both compilation and the dependency-identity fixed point.
  # A separate setup build creates two producers for one artifact, releases the
  # Rust-heavy lease between them, and lets the authoritative freshness build
  # queue behind unrelated work after setup has already compiled the binary.
  log "Building and proving the canonical Harn CLI"
  HARN_BIN='' HARN_BIN_NO_BUILD=0 ./scripts/harn_bin.sh --print >/dev/null
  # Signing mutates the canonical artifact identity, so refresh the receipt
  # only after the final bytes are in place and before publishing a target seed.
  ./scripts/sign_local_macos.sh
  HARN_BIN='' HARN_BIN_NO_BUILD=0 ./scripts/harn_bin.sh --record-receipt
  if [[ -x ./scripts/cargo_target_seed.sh ]] \
    && grep -Fxq "target-dir = \"${cargo_target_root}\" # ${MANAGED_CARGO_CONFIG_MARKER}" \
      .cargo/config.toml; then
    ./scripts/cargo_target_seed.sh publish "${cargo_target_root}" "${HARN_DEV_SETUP_STORAGE_ROOT}"
  fi
else
  echo "Bootstrap profile configured the worktree; deferring compilation to the final task lane."
fi

echo ""
echo "Dev setup complete."
echo "Suggested next commands:"
echo "  make all"
echo "  make portal"
