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
if grep -Fxq -- '--no-sandbox' "$CAPTURE_ARGS"; then exit 1; fi
config_dir="$(sed -n '/^--sandbox-read-root$/{n;p;}' "$CAPTURE_ARGS")"
[[ -n "$config_dir" && ! -e "$config_dir" ]]

unset GODEBUG
export FAKE_STATUS=0
/bin/bash "$fixture/scripts/gh_check_state.sh" --repo example/repo --sha fixture
if grep -Fq 'env:GODEBUG' "$CAPTURE_ARGS"; then exit 1; fi
[[ ! -e "$COMPILER_CALLS" ]]
echo 'gh_check_state_launcher_test: ok'
