#!/usr/bin/env bash
# A machine that turns local hook builds off must start no compiler from the registry selector,
# and the same selector must start one when the switch is off (the control).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# Git config reads must use only the fixture, including inside this worktree.
: > "$tmp/empty-gitconfig"
export GIT_CONFIG_GLOBAL="$tmp/empty-gitconfig" GIT_CONFIG="$tmp/empty-gitconfig"
mkdir "$tmp/shim" "$tmp/target"
for tool in cargo rustc; do
  printf '#!/bin/sh\necho "%s $*" >> "%s/compiler.log"\nexit 99\n' "$tool" "$tmp" > "$tmp/shim/$tool"
  chmod +x "$tmp/shim/$tool"
done
printf '%s\n' 'crates/harn-vm/src/lib.rs' > "$tmp/changed"

select_registry_bin() {
  : > "$tmp/compiler.log"
  (
    cd "$root"
    unset HARN_BIN
    export PATH="$tmp/shim:$PATH" CARGO_TARGET_DIR="$tmp/target" "$@"
    # shellcheck source=/dev/null
    source .githooks/lib.sh
    hook_export_registry_harn_bin "$tmp/changed"
  ) > "$tmp/out" 2>&1 || true
  grep -c . "$tmp/compiler.log" || true
}

control="$(select_registry_bin HARN_HOOKS_NO_LOCAL_BUILD=0)"
if [[ "$control" -lt 1 ]]; then
  echo "control: the selector started no compiler with local builds on, so this test proves nothing" >&2
  cat "$tmp/out" >&2
  exit 1
fi

for setting in HARN_HOOKS_NO_LOCAL_BUILD=1; do
  started="$(select_registry_bin "$setting")"
  if [[ "$started" -ne 0 ]]; then
    echo "$setting still started $started compiler invocation(s)" >&2
    cat "$tmp/compiler.log" >&2
    exit 1
  fi
done

# The git setting reaches the same gate through a private config file.
printf '[harn "hooks"]\n\tnoLocalBuild = true\n' > "$tmp/gitconfig"
started="$(select_registry_bin GIT_CONFIG="$tmp/gitconfig" HARN_HOOKS_NO_LOCAL_BUILD=0)"
if [[ "$started" -ne 0 ]]; then
  echo "git config harn.hooks.noLocalBuild still started $started compiler invocation(s)" >&2
  exit 1
fi

# The pre-push census reports a skip, never a pass, when builds are off.
grep -q 'SKIPPING agent gate census' "$root/.githooks/pre-push"
echo "hook_no_local_build_test: ok (control started $control compiler invocation(s))"

# A gate behind the build switch (any non-census gate) skips loudly instead of reaching the build, even
# when the machine also asks for the full local run; with builds on, the same gate runs (the control).
gate_decision() {
  (
    cd "$root"
    export "$@"
    # shellcheck source=/dev/null
    source .githooks/lib.sh
    if hook_skip_no_local_build "pre-push generated-artifact registry check" 2> "$tmp/gate.err" > "$tmp/gate.out"; then
      echo skipped
    else
      echo ran
    fi
  )
}
if [[ "$(gate_decision HARN_HOOKS_FULL_LOCAL=1 HARN_HOOKS_NO_LOCAL_BUILD=0)" != ran ]]; then
  echo "control: with builds on and the full local run asked for, the gate must run" >&2
  exit 1
fi
if [[ "$(gate_decision HARN_HOOKS_FULL_LOCAL=1 HARN_HOOKS_NO_LOCAL_BUILD=1)" != skipped ]]; then
  echo "a gate still ran with local builds off" >&2
  exit 1
fi
grep -q 'SKIPPING pre-push generated-artifact registry check' "$tmp/gate.err" || {
  echo "the skip was not loud" >&2
  exit 1
}
(
  cd "$root"
  export HARN_HOOKS_FULL_LOCAL=1 HARN_HOOKS_NO_LOCAL_BUILD=1
  # shellcheck source=/dev/null
  source .githooks/lib.sh
  hook_fast_default_mode
) || { echo "builds off must force the fast default so pre-push exits before its full local checks" >&2; exit 1; }
echo "hook_no_local_build_test: non-census gate skips loudly"
