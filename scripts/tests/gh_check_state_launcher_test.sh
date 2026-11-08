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

# The real resolver must classify an unavailable binary without compiling.
cat > "$tmp_root/bin/cargo" <<'SH'
#!/bin/sh
printf 'cargo\n' >> "$COMPILER_CALLS"
exit 91
SH
cp "$tmp_root/bin/cargo" "$fixture/scripts/cargo_with_worktree_build_dir.sh"
chmod +x "$tmp_root/bin/cargo" "$fixture/scripts/cargo_with_worktree_build_dir.sh"
export COMPILER_CALLS="$tmp_root/compiler-calls"
export PATH="$tmp_root/bin:$PATH"
export CARGO_TARGET_DIR="$fixture/target"
export HARN_CARGO_LEASE_MODE=off HARN_BIN_NO_BUILD=0 GH_TOKEN=fixture-token
unset HARN_BIN
set +e
/bin/bash "$fixture/scripts/gh_check_state.sh" --repo example/repo --sha fixture > "$tmp_root/missing.log" 2>&1
status=$?
set -e
[[ ! -e "$COMPILER_CALLS" && "$status" == 3 ]]
grep -Fq 'no check census was measured' "$tmp_root/missing.log"

cat > "$tmp_root/bin/fake-harn" <<'SH'
#!/bin/sh
for argument in "$@"; do
  case "$argument" in
    */gh_check_state_contract.harn)
      [ "$GH_TOKEN" = gh-check-state-contract-canary ] || exit 92
      printf 'probe\n' >> "$PROBE_CALLS"
      case "$PROBE_MODE" in
        fail) exit 3 ;;
        empty) exit 0 ;;
        malformed) printf '{'; exit 0 ;;
        wrong) printf '{"contract":"wrong"}'; exit 0 ;;
      esac
      printf '{"contract":"harn.protected-command-grants.v1"}\n'
      exit 0 ;;
  esac
done
[ "$GH_TOKEN" = fixture-token ] || exit 93
printf '%s\n' "$@" > "$CAPTURE_ARGS"
exit "$FAKE_STATUS"
SH
chmod +x "$tmp_root/bin/fake-harn"
export HARN_BIN="$tmp_root/bin/fake-harn" CAPTURE_ARGS="$tmp_root/args"
export PROBE_CALLS="$tmp_root/probe-calls" PROBE_MODE=valid FAKE_STATUS=0
# A native executable is accepted; this fixture tests launcher routing only.
cp /usr/bin/true "$tmp_root/bin/gh"
export GODEBUG=netdns=go
for expected in 0 1 2 3 64; do
  export FAKE_STATUS="$expected"
  set +e
  /bin/bash "$fixture/scripts/gh_check_state.sh" --repo example/repo --sha fixture
  status=$?
  set -e
  [[ "$status" == "$expected" ]]
done
grep -Fxq 'gh_token=env:GH_TOKEN,expose=GH_TOKEN,for=gh' "$CAPTURE_ARGS"
grep -Fxq -- '--environment-policy' "$CAPTURE_ARGS"
grep -Fxq 'granted' "$CAPTURE_ARGS"
grep -Fxq "$(realpath "$tmp_root/bin/gh")" "$CAPTURE_ARGS"
grep -Fxq -- '--allow-process-network' "$CAPTURE_ARGS"
if grep -Eq 'gh_config=|gh_dns=|gh_budget_|--sandbox-write-root|--no-sandbox' "$CAPTURE_ARGS"; then exit 1; fi
config_dir="$(awk '/^--sandbox-read-root$/{getline; value=$0} END{print value}' "$CAPTURE_ARGS")"
[[ -n "$config_dir" && ! -e "$config_dir" ]]

# Unsupported binaries cannot reach the real invocation, even on exit zero.
for mode in fail empty malformed wrong; do
  export PROBE_MODE="$mode"
  rm -f "$CAPTURE_ARGS"
  set +e
  /bin/bash "$fixture/scripts/gh_check_state.sh" > "$tmp_root/$mode.log" 2>&1
  status=$?
  set -e
  [[ "$status" == 3 && ! -e "$CAPTURE_ARGS" ]]
  grep -Fq 'no check census was measured' "$tmp_root/$mode.log"
done

# A script wrapper is refused before execution, probe, or credential delivery.
cat > "$tmp_root/bin/gh" <<'SH'
#!/bin/sh
printf 'wrapper\n' >> "$WRAPPER_CALLS"
exit 97
SH
chmod +x "$tmp_root/bin/gh"
export WRAPPER_CALLS="$tmp_root/wrapper-calls" PROBE_MODE=valid
rm -f "$PROBE_CALLS"
set +e
/bin/bash "$fixture/scripts/gh_check_state.sh" > "$tmp_root/wrapper.log" 2>&1
status=$?
set -e
[[ "$status" == 3 && ! -e "$WRAPPER_CALLS" && ! -e "$PROBE_CALLS" ]]
grep -Fq 'scripted GitHub CLI wrappers' "$tmp_root/wrapper.log"
[[ ! -e "$COMPILER_CALLS" ]]
echo 'gh_check_state_launcher_test: ok'
