#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
hook_helper="$script_dir/claude_dev_setup_hook.harn"
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

log_path="${state_dir}/setup-$(date -u +%Y%m%dT%H%M%SZ).log"
start_time="$(date +%s)"

set +e
HARN_DEV_TARGET_WORKTREE_PATH="${HARN_DEV_TARGET_WORKTREE_PATH:-$root}" \
  ./scripts/dev_setup.sh >"$log_path" 2>&1
status=$?
set -e

duration="$(( $(date +%s) - start_time ))"

if [[ "$status" -eq 0 ]]; then
  touch "$stamp"
  ln -sf "$(basename "$log_path")" "${state_dir}/latest.log"
  emit_context "Project dev setup completed in ${duration}s. Log: ${root}/${log_path}"
else
  emit_context "Project dev setup failed after ${duration}s with exit ${status}. Read ${root}/${log_path} before assuming dependencies are ready."
fi

exit 0
