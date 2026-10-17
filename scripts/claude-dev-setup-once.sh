#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

# Background warm phase, re-entered by the session hook below. It owns the
# lock for as long as it runs, and stamps the fingerprint only on success so
# a failed warm is retried by the next session.
if [[ "${1:-}" == "--warm" ]]; then
  warm_root="$2"
  warm_stamp="$3"
  warm_lock_dir="$4"
  warm_log="$5"
  trap 'rm -rf "$warm_lock_dir"' EXIT
  cd "$warm_root"
  # The cloud warm skips setup's tool installs, and `make test` requires
  # nextest. Install CI's pinned release binary next to cargo instead of
  # compiling it.
  if [[ "${CLAUDE_CODE_REMOTE:-}" == "true" ]] && ! command -v cargo-nextest >/dev/null 2>&1; then
    nextest_version="$(jq -er '.nextest_version' .github/cache-policy.json)" &&
      nextest_dir="$(dirname "$(command -v cargo)")" &&
      curl -fsSL "https://get.nexte.st/${nextest_version}/linux" |
      tar -xzf - -C "$nextest_dir" >>"$warm_log" 2>&1 ||
      echo "warning: could not install cargo-nextest ${nextest_version:-}; make test will ask for it" >>"$warm_log"
  fi
  if HARN_DEV_TARGET_WORKTREE_PATH="$warm_root" \
    ./scripts/dev_setup.sh >>"$warm_log" 2>&1; then
    touch "$warm_stamp"
    ln -sf "$(basename "$warm_log")" "$(dirname "$warm_stamp")/latest.log"
  fi
  exit 0
fi

# Claude Code sets CLAUDE_PROJECT_DIR for every hook, so the hook input is not
# needed. Drain it so the host never sees a closed pipe.
cat >/dev/null
root="${CLAUDE_PROJECT_DIR:-$PWD}"
root="$(git -C "$root" rev-parse --show-toplevel 2>/dev/null || printf '%s\n' "$root")"
cd "$root"

if [[ ! -x scripts/dev_setup.sh ]]; then
  exit 0
fi

# A cloud session runs on a 4 vCPU, 16 GB VM with no swap and a 30 GB disk
# allowance, discarded with the session. Measured there, Cargo's one compiler
# per core ran the VM out of memory, and so did CI's budget of two: a harn-vm
# edit rebuilds the library and its unit tests at once, and each rustc peaks
# near 10 GB. Two compilers plus an 8 GB swap file ran build, VM and CLI tests,
# and two edit rounds with no kill. Incremental state was half of the target
# dir and pushed free disk to 4 GB, so it stays off, at a cost of about six
# minutes per harn-vm edit round. Debuginfo is off as in CI's Rust lanes, which
# also lets test builds reuse the warmed dev graph.
cloud_rust_env=()
cloud_notes=""
if [[ "${CLAUDE_CODE_REMOTE:-}" == "true" ]]; then
  # shellcheck source=scripts/ci/rust_resource_budget.sh
  source "$script_dir/ci/rust_resource_budget.sh"
  cloud_cores="$(host_cpu_cores)"
  cloud_jobs="$(rust_resource_budget "$script_dir/ci/rust-resource-policy.json" \
    "$cloud_cores" 1 producer "$(host_memory_mb "$cloud_cores")" 2>/dev/null \
    | sed -n 's/^build_jobs=//p')" || cloud_jobs=""
  # An unmeasured budget must not fall back to Cargo's default, which is the
  # out-of-memory configuration this exists to prevent.
  cloud_rust_env=(
    CARGO_BUILD_JOBS="${cloud_jobs:-1}"
    CARGO_INCREMENTAL=0
    CARGO_PROFILE_DEV_DEBUG=0
  )
  export "${cloud_rust_env[@]}"
  cloud_swap=/swapfile
  if [[ "$(uname -s)" == Linux && "$(id -u)" == 0 ]] &&
    ! awk 'NR > 1 { found = 1 } END { exit !found }' /proc/swaps; then
    if ! { fallocate -l 8G "$cloud_swap" && chmod 600 "$cloud_swap" &&
      mkswap "$cloud_swap" && swapon "$cloud_swap"; } >/dev/null 2>&1; then
      rm -f "$cloud_swap"
      cloud_notes=" No swap could be added, so two large compiles at once can still be killed for memory."
    fi
  fi
fi

persist_env() {
  if [[ -n "${CLAUDE_ENV_FILE:-}" ]]; then
    printf 'export HARN_DEV_TARGET_WORKTREE_PATH=%q\n' "$root" >> "$CLAUDE_ENV_FILE"
    local setting
    for setting in ${cloud_rust_env[@]+"${cloud_rust_env[@]}"}; do
      printf 'export %q\n' "$setting" >> "$CLAUDE_ENV_FILE"
    done
  fi
}

# SessionStart adds plain stdout to the session as context. Plain text needs no
# Harn runtime, which a fresh clone does not have yet.
emit_context() {
  printf '%s\n' "$1"
}

# A cloud session's VM is discarded with the session, so optional tool installs
# and frontend builds there are pure cost. Warm only the Rust graph.
warm_profile="full"
warm_work="tool installs, portal build, workspace check, signing"
if [[ "${CLAUDE_CODE_REMOTE:-}" == "true" ]]; then
  warm_profile="rust"
  warm_work="nextest install, workspace check; Cargo runs ${CARGO_BUILD_JOBS} compilers with incremental off to fit this VM's memory and disk.${cloud_notes}"
fi

fingerprint="$(
  {
    printf 'claude-dev-setup-once:v1\n'
    for path in \
      scripts/claude-dev-setup-once.sh \
      scripts/dev_setup.sh \
      Cargo.lock \
      package-lock.json \
      crates/harn-cli/portal/package-lock.json \
      tree-sitter-harn/package-lock.json \
      editors/vscode/package-lock.json \
      website/package-lock.json
    do
      [[ -f "$path" ]] && shasum -a 256 "$path"
    done
    true
  } | shasum -a 256 | awk '{print $1}'
)"

state_dir=".claude/dev-setup"
stamp="${state_dir}/${fingerprint}.stamp"
mkdir -p "$state_dir"

persist_env
if [[ -f "$stamp" && "${CLAUDE_DEV_SETUP_FORCE:-0}" != "1" ]]; then
  exit 0
fi

# Claude blocks the session on this hook, so it must not run the full setup
# inline. The fingerprint above covers Cargo.lock and every package-lock.json,
# all of which move on main several times a day, so most sessions started from
# a tracking checkout re-arm the expensive phases; they have blocked real
# sessions here for 104s, 220s, and 432s.
#
# Split the work the way dev_setup.sh already models it. The bootstrap profile
# does what an agent needs before its first Cargo probe -- git hooks, merge
# drivers, and the per-worktree target dir -- and compiles nothing. The
# expensive phases (tool installs, portal build, workspace check, signing)
# warm in the background, where a slow build costs throughput instead of a
# stalled session.
log_path="${state_dir}/setup-$(date -u +%Y%m%dT%H%M%SZ).log"
start_time="$(date +%s)"

set +e
HARN_DEV_SETUP_PROFILE=bootstrap \
HARN_DEV_TARGET_WORKTREE_PATH="${HARN_DEV_TARGET_WORKTREE_PATH:-$root}" \
  ./scripts/dev_setup.sh >"$log_path" 2>&1
status=$?
set -e

duration="$(( $(date +%s) - start_time ))"

if [[ "$status" -ne 0 ]]; then
  emit_context "Project dev setup could not configure this worktree (exit ${status} after ${duration}s). Read ${root}/${log_path} before assuming dependencies are ready."
  exit 0
fi

# One warm run per checkout. Sessions start here in bursts, and dev_setup.sh
# phases like the portal build and npm installs write shared trees that do not
# survive concurrent runs. A lock directory left behind by a killed warm goes
# stale after an hour so warming cannot wedge permanently.
lock_dir="${state_dir}/warm.lock"
if [[ -d "$lock_dir" ]] && [[ -n "$(find "$lock_dir" -maxdepth 0 -mmin +60 2>/dev/null)" ]]; then
  rm -rf "$lock_dir"
fi

if ! mkdir "$lock_dir" 2>/dev/null; then
  emit_context "Worktree configured in ${duration}s. A background dev setup from another session is still warming dependencies, so the first build may block until it finishes."
  exit 0
fi

HARN_DEV_SETUP_PROFILE="$warm_profile" nohup "$script_dir/claude-dev-setup-once.sh" --warm \
  "$root" "$stamp" "$lock_dir" "$log_path" </dev/null >/dev/null 2>&1 &
warm_pid=$!
disown "$warm_pid" 2>/dev/null || true

emit_context "Worktree configured in ${duration}s. Dependency warming (${warm_work}) is running in the background as pid ${warm_pid}; reading code is safe now, but the first build may block until it finishes, and running make setup now would race it. Log: ${root}/${log_path}"

exit 0
