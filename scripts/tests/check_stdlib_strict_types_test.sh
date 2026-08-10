#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp_root="$(mktemp -d)"
trap 'rm -rf "$tmp_root"' EXIT

fake_harn="$tmp_root/harn"
cat > "$fake_harn" <<'SH'
#!/usr/bin/env bash
cat "${FAKE_HARN_DIAGNOSTICS:?}"
exit "${FAKE_HARN_STATUS:-1}"
SH
chmod +x "$fake_harn"

run_gate() {
  local diagnostics="$1"
  HARN_BIN="$fake_harn" \
    FAKE_HARN_DIAGNOSTICS="$diagnostics" \
    "$repo_root/scripts/check_stdlib_strict_types.sh"
}

empty="$tmp_root/empty"
: > "$empty"
run_gate "$empty" > "$tmp_root/empty.out"
grep -Fq "stdlib type-safety gate passed" "$tmp_root/empty.out"

type_error="$tmp_root/type-error"
cat > "$type_error" <<'EOF'
error[HARN-TYP-006]: expected Transcript, found nil
   --> crates/harn-stdlib/src/stdlib/cli/try.harn:52:23
EOF
if run_gate "$type_error" > "$tmp_root/type.out" 2> "$tmp_root/type.err"; then
  echo "ordinary HARN-TYP error unexpectedly passed" >&2
  exit 1
fi
grep -Fq "ordinary type errors are not baseline-eligible" "$tmp_root/type.err"
grep -Fq "cli/try.harn:52:23" "$tmp_root/type.err"

excluded_ownership="$tmp_root/excluded-ownership"
cat > "$excluded_ownership" <<'EOF'
warning[HARN-OWN-004]: unvalidated boundary value used directly
   --> crates/harn-stdlib/src/stdlib/agent/user.harn:10:2
EOF
run_gate "$excluded_ownership" > "$tmp_root/excluded.out"

owned_violation="$tmp_root/owned-violation"
cat > "$owned_violation" <<'EOF'
warning[HARN-OWN-004]: unvalidated boundary value used directly
   --> crates/harn-stdlib/src/stdlib/cli/try.harn:10:2
EOF
if run_gate "$owned_violation" > "$tmp_root/owned.out" 2> "$tmp_root/owned.err"; then
  echo "unratcheted HARN-OWN-004 warning unexpectedly passed" >&2
  exit 1
fi
grep -Fq "cli/try.harn:10:2" "$tmp_root/owned.err"

echo "check_stdlib_strict_types_test: ok"
