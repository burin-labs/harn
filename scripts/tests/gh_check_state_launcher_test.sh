#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
tmp_root="$(mktemp -d)"
trap 'rm -rf "$tmp_root"' EXIT
fixture="$tmp_root/repo"
mkdir -p "$fixture/scripts/lib" "$tmp_root/bin" "$fixture/target"
cp "$repo_root/scripts/gh_check_state.sh" "$repo_root/scripts/harn_bin.sh" "$fixture/scripts/"
for library in cargo_env harn_bin harn_bin_freshness sha256; do
  cp "$repo_root/scripts/lib/$library.sh" "$fixture/scripts/lib/"
done

# Use the real resolver and a compiler tripwire, never a real cold build.
cat > "$tmp_root/bin/cargo" <<'SH'
#!/bin/sh
printf 'cargo\n' >> "$COMPILER_CALLS"
exit 91
SH
cat > "$fixture/scripts/cargo_with_worktree_build_dir.sh" <<'SH'
#!/bin/sh
printf 'compiler\n' >> "$COMPILER_CALLS"
exit 91
SH
chmod +x "$tmp_root/bin/cargo" "$fixture/scripts/cargo_with_worktree_build_dir.sh"
export COMPILER_CALLS="$tmp_root/compiler-calls"
export PATH="$tmp_root/bin:$PATH"
export CARGO_TARGET_DIR="$fixture/target"
export HARN_CARGO_LEASE_MODE=off
export HARN_BIN_NO_BUILD=0
export GH_TOKEN=fixture-token
unset HARN_BIN

set +e
/bin/bash "$fixture/scripts/gh_check_state.sh" --repo example/repo --sha fixture > "$tmp_root/missing.log" 2>&1
status=$?
set -e
[[ ! -e "$COMPILER_CALLS" ]] || { echo 'status read invoked a compiler' >&2; exit 1; }
[[ "$status" == 3 ]] || { cat "$tmp_root/missing.log"; exit 1; }
grep -Fq 'no check census was measured' "$tmp_root/missing.log"

cat > "$tmp_root/bin/fake-harn" <<'SH'
#!/bin/sh
printf '%s\n' "$@" > "$CAPTURE_ARGS"
exit "$FAKE_STATUS"
SH
chmod +x "$tmp_root/bin/fake-harn"
export HARN_BIN="$tmp_root/bin/fake-harn"
export CAPTURE_ARGS="$tmp_root/args"
export POLICY_CALLS="$tmp_root/policy-calls"
export POLICY_MODE=valid
export POLICY_IMPLEMENTATION="$tmp_root/implementation.py"
export POLICY_STATE="$tmp_root/budget-state"
export POLICY_REAL="$(realpath /usr/bin/true)"
touch "$POLICY_IMPLEMENTATION"
cat > "$tmp_root/bin/gh" <<'SH'
#!/bin/bash
printf '%s:%s\n' "${GH_BUDGET_PRINT_REAL:-}" "$*" >> "$POLICY_CALLS"
[[ "$*" == --budget-subprocess-policy && "$GH_BUDGET_PRINT_REAL" == 1 ]] || exit 97
case "$POLICY_MODE" in
  unavailable) exit 64 ;;
  old-wrapper) printf '%s\n' "$POLICY_REAL"; exit 0 ;;
  malformed) echo '{'; exit 0 ;;
esac
jq -nc --arg executable "$(realpath "$0")" --arg implementation "$POLICY_IMPLEMENTATION" \
  --arg real "$POLICY_REAL" --arg state "$POLICY_STATE" --arg mode "$POLICY_MODE" '
  {schema:"gh-budget.subprocess-policy.v1", executable:$executable,
   implementation:$implementation, real_executable:$real, state_directory:$state} |
  if $mode == "identity" then .executable = $real
  elif $mode == "schema" then .schema = "unknown"
  elif $mode == "extra" then .arbitrary_root = "/"
  elif $mode == "relative" then .state_directory = "relative"
  elif $mode == "newline" then .state_directory = "/tmp/\nroot"
  elif $mode == "missing" then .real_executable = "/absent/gh"
  else . end'
SH
chmod +x "$tmp_root/bin/gh"
export GODEBUG=netdns=go
for expected in 0 1 2 3 64; do
  export FAKE_STATUS="$expected"
  set +e
  /bin/bash "$fixture/scripts/gh_check_state.sh" --repo example/repo --sha fixture
  status=$?
  set -e
  [[ "$status" == "$expected" ]]
done
grep -Fxq 'gh_dns=env:GODEBUG,expose=GODEBUG,for=gh' "$CAPTURE_ARGS"
grep -Fxq 'gh_token=env:GH_TOKEN,expose=GH_TOKEN,for=gh' "$CAPTURE_ARGS"
grep -Fxq 'gh_config=env:GH_CONFIG_DIR,expose=GH_CONFIG_DIR,for=gh' "$CAPTURE_ARGS"
grep -Fxq -- '--sandbox-read-root' "$CAPTURE_ARGS"
grep -Fxq "$POLICY_IMPLEMENTATION" "$CAPTURE_ARGS"
grep -Fxq "$POLICY_REAL" "$CAPTURE_ARGS"
grep -Fxq -- '--sandbox-write-root' "$CAPTURE_ARGS"
grep -Fxq "$POLICY_STATE" "$CAPTURE_ARGS"
grep -Fxq 'gh_budget_state=env:GH_BUDGET_STATE_DIR,expose=GH_BUDGET_STATE_DIR,for=gh' "$CAPTURE_ARGS"
grep -Fxq 'gh_budget_real=env:GH_BUDGET_REAL_GH,expose=GH_BUDGET_REAL_GH,for=gh' "$CAPTURE_ARGS"
if grep -Fxq -- '--no-sandbox' "$CAPTURE_ARGS"; then exit 1; fi
config_dir="$(awk '/^--sandbox-read-root$/{getline; value=$0} END{print value}' "$CAPTURE_ARGS")"
[[ -n "$config_dir" && ! -e "$config_dir" ]]
[[ ! -e "$POLICY_STATE" ]]

for mode in unavailable old-wrapper malformed identity schema extra relative newline missing; do
  export POLICY_MODE="$mode"
  rm -f "$CAPTURE_ARGS"
  set +e
  /bin/bash "$fixture/scripts/gh_check_state.sh" --repo example/repo --sha fixture > "$tmp_root/$mode.log" 2>&1
  status=$?
  set -e
  [[ "$status" == 3 && ! -e "$CAPTURE_ARGS" ]] || { cat "$tmp_root/$mode.log"; exit 1; }
  grep -Fq 'no check census was measured' "$tmp_root/$mode.log"
done
if grep -Fxv '1:--budget-subprocess-policy' "$POLICY_CALLS"; then exit 1; fi
export POLICY_MODE=valid

unset GODEBUG
export FAKE_STATUS=0
/bin/bash "$fixture/scripts/gh_check_state.sh" --repo example/repo --sha fixture
if grep -Fq 'env:GODEBUG' "$CAPTURE_ARGS"; then exit 1; fi
# A native CLI keeps its own executable and needs no wrapper state or grants.
cp "$POLICY_REAL" "$tmp_root/bin/gh"
/bin/bash "$fixture/scripts/gh_check_state.sh" --repo example/repo --sha fixture
grep -Fxq "$(realpath "$tmp_root/bin/gh")" "$CAPTURE_ARGS"
if grep -Eq 'gh_budget_|--sandbox-write-root' "$CAPTURE_ARGS"; then exit 1; fi
[[ ! -e "$COMPILER_CALLS" ]]
echo 'gh_check_state_launcher_test: ok'
