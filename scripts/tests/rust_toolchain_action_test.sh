#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
installer="$repo_root/.github/actions/rust-toolchain/install.sh"
tmp_root="$(mktemp -d "${TMPDIR:-/tmp}/harn-rust-toolchain-action-test.XXXXXX")"
trap 'rm -rf "$tmp_root"' EXIT
mkdir -p "$tmp_root/bin"

cat > "$tmp_root/bin/rustup" <<'SCRIPT'
#!/usr/bin/env bash
set -euo pipefail
printf 'rustup %s\n' "$*" >> "${TOOLCHAIN_TEST_STATE:?}/calls"
SCRIPT

cat > "$tmp_root/bin/rustc" <<'SCRIPT'
#!/usr/bin/env bash
set -euo pipefail
printf 'rustc %s\n' "$*" >> "${TOOLCHAIN_TEST_STATE:?}/calls"
count_file="${TOOLCHAIN_TEST_STATE}/rustc-count"
count=0
[[ ! -f "$count_file" ]] || count="$(< "$count_file")"
count=$((count + 1))
printf '%s\n' "$count" > "$count_file"
if [[ "${TOOLCHAIN_TEST_MODE:?}" == "rustc-transient" && "$count" -eq 1 ]]; then
  echo 'error: component download failed for cargo: connection reset by peer' >&2
  exit 28
fi
printf 'rustc 1.95.0 (fake)\n'
SCRIPT

cat > "$tmp_root/bin/cargo" <<'SCRIPT'
#!/usr/bin/env bash
set -euo pipefail
printf 'cargo %s\n' "$*" >> "${TOOLCHAIN_TEST_STATE:?}/calls"
count_file="${TOOLCHAIN_TEST_STATE}/cargo-count"
count=0
[[ ! -f "$count_file" ]] || count="$(< "$count_file")"
count=$((count + 1))
printf '%s\n' "$count" > "$count_file"
if [[ "${TOOLCHAIN_TEST_MODE:?}" == "always-fail" || \
  ("${TOOLCHAIN_TEST_MODE}" == "cargo-transient" && "$count" -eq 1) ]]; then
  echo 'error: component download failed for cargo: connection reset by peer' >&2
  exit 28
fi
printf 'cargo 1.95.0 (fake)\n'
SCRIPT

cat > "$tmp_root/bin/sleep" <<'SCRIPT'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$1" >> "${TOOLCHAIN_TEST_STATE:?}/sleeps"
SCRIPT

chmod +x "$tmp_root/bin/"*

run_case() {
  local name=$1 mode=$2 expected_status=$3
  local state="$tmp_root/$name"
  mkdir -p "$state"
  local status=0
  PATH="$tmp_root/bin:$PATH" \
    TOOLCHAIN_TEST_STATE="$state" \
    TOOLCHAIN_TEST_MODE="$mode" \
    EXTRA_COMPONENTS='rustfmt, clippy' \
    EXTRA_TARGETS='x86_64-pc-windows-msvc' \
    "$installer" > "$state/output" 2>&1 || status=$?
  if [[ "$status" -ne "$expected_status" ]]; then
    printf '%s: expected status %s, got %s\n' "$name" "$expected_status" "$status" >&2
    cat "$state/output" >&2
    exit 1
  fi
}

run_case rustc-transient rustc-transient 0
diff -u - "$tmp_root/rustc-transient/calls" <<'EXPECTED'
rustup show
rustup component add rustfmt clippy
rustup target add x86_64-pc-windows-msvc
rustc -Vv
rustc -Vv
cargo -V
EXPECTED
grep -Fq '::warning::Rust toolchain transport failed (attempt 1/4); retrying in 2s: rustc -Vv' \
  "$tmp_root/rustc-transient/output"
grep -Fxq '2' "$tmp_root/rustc-transient/sleeps"

run_case cargo-transient cargo-transient 0
diff -u - "$tmp_root/cargo-transient/calls" <<'EXPECTED'
rustup show
rustup component add rustfmt clippy
rustup target add x86_64-pc-windows-msvc
rustc -Vv
cargo -V
cargo -V
EXPECTED
grep -Fq '::warning::Rust toolchain transport failed (attempt 1/4); retrying in 2s: cargo -V' \
  "$tmp_root/cargo-transient/output"
grep -Fxq '2' "$tmp_root/cargo-transient/sleeps"

run_case permanent always-fail 1
[[ "$(grep -Fc 'cargo -V' "$tmp_root/permanent/calls")" -eq 4 ]]
[[ "$(paste -sd, "$tmp_root/permanent/sleeps")" == '2,4,8' ]]
grep -Fq 'Rust toolchain command failed after 4 attempts: cargo -V' \
  "$tmp_root/permanent/output"

echo 'rust toolchain action tests passed'
