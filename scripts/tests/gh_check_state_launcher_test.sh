#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
tmp_root="$(mktemp -d)"
tmp_root="$(realpath "$tmp_root")"
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
export REFRESH_CALLS="$tmp_root/refresh-calls"
export REFRESH_FAIL=0
export GITHUB_TOKEN=must-not-reach-refresh
export GODEBUG=must-not-reach-child
implementation="$tmp_root/bin/implementation.py"
state="$tmp_root/budget-state"
native="$(realpath /usr/bin/true)"
interpreter="$(realpath "$(command -v python3)")"
mkdir "$state"
cat > "$implementation" <<'PY'
import os
from pathlib import Path
import sys
assert sys.argv[1:] == ["--install-owner"]
assert "GH_TOKEN" not in os.environ and "GITHUB_TOKEN" not in os.environ
assert os.environ["GH_DEBUG"] == ""
assert os.environ["GH_BUDGET_PRINT_REAL"] == ""
assert os.environ["GH_CONFIG_DIR"].endswith("/accounts")
assert Path(os.environ["GH_BUDGET_REAL_GH"]).is_file()
assert Path(os.environ["GH_BUDGET_STATE_DIR"]).is_dir()
with Path(os.environ["REFRESH_CALLS"]).open("a") as stream:
    stream.write("refreshed\n")
sys.exit(int(os.environ["REFRESH_FAIL"]))
PY
cat > "$tmp_root/bin/gh" <<'SH'
#!/bin/bash
echo 'launcher executed the unvalidated wrapper' >&2
exit 97
SH
chmod +x "$tmp_root/bin/gh" "$implementation"
source "$repo_root/scripts/lib/sha256.sh"
owner="$tmp_root/bin/.harn-github-budget-owner.json"
records='{}'
for pair in "wrapper:$tmp_root/bin/gh" "implementation:$implementation" "adapter:$implementation" \
  "interpreter:$interpreter" "native_gh:$native"; do
  role="${pair%%:*}"
  file="${pair#*:}"
  digest="$(sha256_file_hex "$file")"
  records="$(jq -nc --argjson records "$records" --arg role "$role" --arg path "$file" --arg sha256 "$digest" \
    '$records + {($role):{path:$path,sha256:$sha256}}')"
done
jq -nc --argjson records "$records" --arg state "$state" --arg account "$tmp_root/accounts/hosts.yml" \
  '$records + {schema:"github.budget-owner.v1",state_directory:$state,account_config:$account,policy_environment:{}}' > "$owner"
cp "$owner" "$tmp_root/valid-owner.json"
for expected in 0 1 2 3 64; do
  export FAKE_STATUS="$expected"
  set +e
  /bin/bash "$fixture/scripts/gh_check_state.sh" --repo example/repo --sha fixture
  status=$?
  set -e
  [[ "$status" == "$expected" ]]
done
grep -Fxq 'gh_token=env:GH_TOKEN,expose=GH_TOKEN,for=gh' "$CAPTURE_ARGS"
grep -Fxq -- '--sandbox-read-root' "$CAPTURE_ARGS"
[[ "$(wc -l < "$REFRESH_CALLS" | tr -d ' ')" == 5 ]]
if grep -Eq 'gh_budget_|gh_config=|gh_dns=|--sandbox-write-root|--no-sandbox' "$CAPTURE_ARGS"; then exit 1; fi

for mode in unavailable malformed identity schema extra relative newline missing digest refresh; do
  cp "$tmp_root/valid-owner.json" "$owner"
  case "$mode" in
    unavailable) rm "$owner" ;;
    malformed) printf '{' > "$owner" ;;
    refresh) export REFRESH_FAIL=78 ;;
    *) jq --arg mode "$mode" '
      if $mode == "identity" then .wrapper.path = .native_gh.path
      elif $mode == "schema" then .schema = "unknown"
      elif $mode == "extra" then .arbitrary_root = "/"
      elif $mode == "relative" then .state_directory = "relative"
      elif $mode == "newline" then .state_directory = "/tmp/\nroot"
      elif $mode == "missing" then .native_gh.path = "/absent/gh"
      elif $mode == "digest" then .adapter.sha256 = ("0" * 64)
      else error("unhandled refusal fixture") end
      ' "$tmp_root/valid-owner.json" > "$owner" ;;
  esac
  rm -f "$CAPTURE_ARGS"
  set +e
  /bin/bash "$fixture/scripts/gh_check_state.sh" --repo example/repo --sha fixture > "$tmp_root/$mode.log" 2>&1
  status=$?
  set -e
  [[ "$status" == 3 && ! -e "$CAPTURE_ARGS" ]] || { cat "$tmp_root/$mode.log"; exit 1; }
  grep -Fq 'no check census was measured' "$tmp_root/$mode.log"
done
export REFRESH_FAIL=0
cp "$tmp_root/valid-owner.json" "$owner"

unset GODEBUG
export FAKE_STATUS=0
/bin/bash "$fixture/scripts/gh_check_state.sh" --repo example/repo --sha fixture
if grep -Fq 'env:GODEBUG' "$CAPTURE_ARGS"; then exit 1; fi
# A native CLI keeps its own executable and needs no wrapper state or grants.
cp "$native" "$tmp_root/bin/gh"
/bin/bash "$fixture/scripts/gh_check_state.sh" --repo example/repo --sha fixture
grep -Fxq "$(realpath "$tmp_root/bin/gh")" "$CAPTURE_ARGS"
if grep -Eq 'gh_budget_|--sandbox-write-root' "$CAPTURE_ARGS"; then exit 1; fi
[[ ! -e "$COMPILER_CALLS" ]]
echo 'gh_check_state_launcher_test: ok'
