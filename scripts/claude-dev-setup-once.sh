#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
hook_helper="$script_dir/claude_dev_setup_hook.harn"

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
  if HARN_DEV_TARGET_WORKTREE_PATH="$warm_root" \
    ./scripts/dev_setup.sh >>"$warm_log" 2>&1; then
    touch "$warm_stamp"
    ln -sf "$(basename "$warm_log")" "$(dirname "$warm_stamp")/latest.log"
  fi
  exit 0
fi

input="$(cat)"

resolve_hook_harn() {
  "$script_dir/harn_bin.sh" --no-build --print 2>/dev/null || true
}

hook_harn="$(resolve_hook_harn)"
hook_cwd=""
if [[ -n "$hook_harn" ]]; then
  hook_cwd="$(printf '%s' "$input" | "$hook_harn" run "$hook_helper" -- --cwd 2>/dev/null || true)"
fi
root="${CLAUDE_PROJECT_DIR:-${hook_cwd:-$PWD}}"
root="$(git -C "$root" rev-parse --show-toplevel 2>/dev/null || printf '%s\n' "$root")"
cd "$root"

if [[ ! -x scripts/dev_setup.sh ]]; then
  exit 0
fi

persist_env() {
  if [[ -n "${CLAUDE_ENV_FILE:-}" ]]; then
    printf 'export HARN_DEV_TARGET_WORKTREE_PATH=%q\n' "$root" >> "$CLAUDE_ENV_FILE"
  fi
}

emit_context() {
  local message="$1"
  if [[ -z "$hook_harn" ]]; then
    hook_harn="$(resolve_hook_harn)"
  fi
  if [[ -n "$hook_harn" ]]; then
    "$hook_harn" run "$hook_helper" -- --context "$message" 2>/dev/null || true
  fi
}

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

nohup "$script_dir/claude-dev-setup-once.sh" --warm \
  "$root" "$stamp" "$lock_dir" "$log_path" </dev/null >/dev/null 2>&1 &
warm_pid=$!
disown "$warm_pid" 2>/dev/null || true

emit_context "Worktree configured in ${duration}s. Dependency warming (tool installs, portal build, workspace check, signing) is running in the background as pid ${warm_pid}; reading code is safe now, but the first build may block until it finishes. Log: ${root}/${log_path}"

exit 0
