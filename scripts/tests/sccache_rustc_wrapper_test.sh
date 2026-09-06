#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

wrapper_source="$repo_root/scripts/sccache_rustc_wrapper.rs"
wrapper_binary="$tmp_root/harn-sccache-wrapper"
wrapper_tests="$tmp_root/harn-sccache-wrapper-tests"
case "${OS:-$(uname -s)}" in
  Windows_NT | MINGW* | MSYS* | CYGWIN*)
    wrapper_binary="${wrapper_binary}.exe"
    wrapper_tests="${wrapper_tests}.exe"
    ;;
esac

rustc --edition=2021 --test "$wrapper_source" -o "$wrapper_tests"
"$wrapper_tests"
rustc --edition=2021 "$wrapper_source" -o "$wrapper_binary"

if [[ "${OS:-$(uname -s)}" != Windows_NT \
  && "${OS:-$(uname -s)}" != MINGW* \
  && "${OS:-$(uname -s)}" != MSYS* \
  && "${OS:-$(uname -s)}" != CYGWIN* ]]; then
  lifecycle_bin="$tmp_root/lifecycle-bin"
  lifecycle_ready="$tmp_root/lifecycle-ready"
  lifecycle_release="$tmp_root/lifecycle-release"
  mkdir -p "$lifecycle_bin"
  mkfifo "$lifecycle_ready" "$lifecycle_release"
  cat > "$lifecycle_bin/rustc-probe" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$$" > "$LIFECYCLE_READY"
IFS= read -r _ < "$LIFECYCLE_RELEASE"
SH
  cp "$lifecycle_bin/rustc-probe" "$lifecycle_bin/sccache"
  chmod +x "$lifecycle_bin/rustc-probe" "$lifecycle_bin/sccache"

  assert_process_replaced() {
    local route="$1"
    local wrapper_pid compiler_pid
    if [[ "$route" == direct ]]; then
      CARGO_BIN_EXE_probe=placeholder \
        LIFECYCLE_READY="$lifecycle_ready" \
        LIFECYCLE_RELEASE="$lifecycle_release" \
        "$wrapper_binary" "$lifecycle_bin/rustc-probe" &
    else
      PATH="$lifecycle_bin:$PATH" \
        LIFECYCLE_READY="$lifecycle_ready" \
        LIFECYCLE_RELEASE="$lifecycle_release" \
        "$wrapper_binary" "$lifecycle_bin/rustc-probe" &
    fi
    wrapper_pid=$!
    IFS= read -r compiler_pid < "$lifecycle_ready"
    if [[ "$compiler_pid" != "$wrapper_pid" ]]; then
      printf 'wrapper did not preserve process identity for %s route: wrapper=%s compiler=%s\n' \
        "$route" "$wrapper_pid" "$compiler_pid" >&2
      return 1
    fi
    printf 'release\n' > "$lifecycle_release"
    wait "$wrapper_pid"
  }

  assert_process_replaced direct
  assert_process_replaced sccache
fi

fixture="$tmp_root/cargo-binary-env"
mkdir -p "$fixture/src" "$fixture/tests"
cat > "$fixture/Cargo.toml" <<'TOML'
[package]
name = "sccache-binary-env-probe"
version = "0.0.0"
edition = "2021"

[[bin]]
name = "probe-bin"
path = "src/main.rs"
TOML
cat > "$fixture/src/main.rs" <<'RS'
fn main() {}
RS
cat > "$fixture/tests/binary_env.rs" <<'RS'
const PROBE_BIN: &str = env!("CARGO_BIN_EXE_probe-bin");

#[test]
fn cargo_binary_environment_reached_rustc() {
    assert!(!PROBE_BIN.is_empty());
}
RS

output="$tmp_root/cargo-output.txt"
CARGO_TARGET_DIR="$tmp_root/target" \
  RUSTC_WRAPPER="$wrapper_binary" \
  HARN_SCCACHE_WRAPPER_TRACE=1 \
  cargo test --manifest-path "$fixture/Cargo.toml" --no-run -vv \
  > "$output" 2>&1

if ! grep -Fq 'harn-sccache-wrapper: route=direct cargo-binary environment' "$output"; then
  echo "real Cargo probe did not route its compiler-provided binary environment directly" >&2
  cat "$output" >&2
  exit 1
fi
if ! grep -Fq 'harn-sccache-wrapper: route=sccache' "$output"; then
  echo "real Cargo probe did not preserve sccache for ordinary compilation" >&2
  cat "$output" >&2
  exit 1
fi

echo "sccache_rustc_wrapper_test: ok"
