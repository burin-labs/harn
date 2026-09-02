#!/usr/bin/env bash
set -euo pipefail

# The cargo wrapper must refuse when a rustc earlier on PATH than the rustup
# shim shadows the pinned toolchain, and must stay out of the way when the
# resolved compiler matches the pin.

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
wrapper="$repo_root/scripts/cargo_with_worktree_build_dir.sh"
pinned=$(sed -n 's/^channel[[:space:]]*=[[:space:]]*"\(.*\)"/\1/p' "$repo_root/rust-toolchain.toml" | head -n 1)
[[ -n "$pinned" ]] || { echo "FAIL: rust-toolchain.toml declares no channel" >&2; exit 1; }

tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

fake_bin="$tmp_root/bin"
mkdir -p "$fake_bin"
cat > "$fake_bin/cargo" <<'SH'
#!/usr/bin/env bash
echo "fake cargo ran"
SH
chmod +x "$fake_bin/cargo"

write_fake_rustc() {
  cat > "$fake_bin/rustc" <<SH
#!/usr/bin/env bash
echo "rustc $1 (deadbeef 2026-01-01)"
SH
  chmod +x "$fake_bin/rustc"
}

run_wrapper() {
  env PATH="$fake_bin:$PATH" HARN_CARGO_LEASE_MODE=off \
    CARGO_TARGET_DIR="$tmp_root/target" "$wrapper" version
}

# A newer compiler first on PATH must refuse, and say enough to act on.
write_fake_rustc "9.99.0"
set +e
shadowed_output=$(run_wrapper 2>&1)
shadowed_status=$?
set -e
[[ "$shadowed_status" -ne 0 ]] \
  || { echo "FAIL: a shadowing rustc did not refuse" >&2; exit 1; }
grep -q "9.99.0" <<<"$shadowed_output" \
  || { echo "FAIL: refusal does not name the resolved compiler" >&2; exit 1; }
grep -q "$pinned" <<<"$shadowed_output" \
  || { echo "FAIL: refusal does not name the pinned compiler" >&2; exit 1; }
grep -q "PATH" <<<"$shadowed_output" \
  || { echo "FAIL: refusal does not name the correction" >&2; exit 1; }
grep -q "fake cargo ran" <<<"$shadowed_output" \
  && { echo "FAIL: Cargo ran anyway under the shadowing compiler" >&2; exit 1; }

# Control: the same wrapper, same fake Cargo, a compiler that matches the pin.
# Without this the refusal above could be any failure at all.
write_fake_rustc "$pinned"
set +e
pinned_output=$(run_wrapper 2>&1)
pinned_status=$?
set -e
[[ "$pinned_status" -eq 0 ]] \
  || { echo "FAIL: pinned compiler refused: $pinned_output" >&2; exit 1; }
grep -q "fake cargo ran" <<<"$pinned_output" \
  || { echo "FAIL: pinned compiler did not reach Cargo" >&2; exit 1; }

# The escape hatch must be explicit and must still reach Cargo.
write_fake_rustc "9.99.0"
set +e
hatch_output=$(env HARN_ALLOW_TOOLCHAIN_MISMATCH=1 PATH="$fake_bin:$PATH" \
  HARN_CARGO_LEASE_MODE=off CARGO_TARGET_DIR="$tmp_root/target" "$wrapper" version 2>&1)
hatch_status=$?
set -e
if [[ "$hatch_status" -ne 0 ]] || ! grep -q "fake cargo ran" <<<"$hatch_output"; then
  echo "FAIL: the documented escape hatch did not reach Cargo" >&2
  exit 1
fi

echo "cargo_toolchain_pin_test: OK"
