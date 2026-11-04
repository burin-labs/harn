#!/usr/bin/env bash
# A machine that turns local hook builds off must start no compiler from the registry selector,
# and the same selector must start one when the switch is off (the control).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

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

# The git setting reaches the same gate. A throwaway global config keeps the real one untouched.
printf '[harn "hooks"]\n\tnoLocalBuild = true\n' > "$tmp/gitconfig"
started="$(select_registry_bin GIT_CONFIG_GLOBAL="$tmp/gitconfig" HARN_HOOKS_NO_LOCAL_BUILD=0)"
if [[ "$started" -ne 0 ]]; then
  echo "git config harn.hooks.noLocalBuild still started $started compiler invocation(s)" >&2
  exit 1
fi

# The pre-push census reports a skip, never a pass, when builds are off.
grep -q 'SKIPPING agent gate census' "$root/.githooks/pre-push"
echo "hook_no_local_build_test: ok (control started $control compiler invocation(s))"
