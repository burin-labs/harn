#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

wrapper_source="$repo_root/scripts/sccache_rustc_wrapper.rs"
wrapper_binary="$tmp_root/harn-sccache-wrapper"
wrapper_tests="$tmp_root/harn-sccache-wrapper-tests"
native_windows=0
case "${OS:-$(uname -s)}" in
  Windows_NT | MINGW* | MSYS* | CYGWIN*)
    native_windows=1
    wrapper_binary="${wrapper_binary}.exe"
    wrapper_tests="${wrapper_tests}.exe"
    ;;
esac

rustc --edition=2021 --test "$wrapper_source" -o "$wrapper_tests"
"$wrapper_tests"
rustc --edition=2021 "$wrapper_source" -o "$wrapper_binary"

if [[ "$native_windows" == 0 ]]; then
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

# The probe asserts which route the wrapper picks, not that sccache caches
# anything, and the sccache route ends in an exec of a program named `sccache`.
# Where a real one is installed, use it and say so. Where none is (the
# repository-policy lane installs no compiler cache), stand up a forwarding
# stub rather than skipping: a probe that quietly does not run is the one
# result this test must never produce.
probe_bin="$tmp_root/probe-bin"
mkdir -p "$probe_bin"
if sccache_binary=$(command -v sccache 2>/dev/null); then
  probe_sccache="installed at $sccache_binary"
else
  sccache_binary=""
  probe_sccache="absent; forwarding compiler probe"
fi
export HARN_TEST_SCCACHE="$sccache_binary"
cat > "$probe_bin/sccache" <<'SH'
#!/usr/bin/env bash
compiler="${1//\\//}"
if [[ -n "${HARN_TEST_SCCACHE_RECORD-}" ]]; then
  printf '%s\n' "$compiler" >> "$HARN_TEST_SCCACHE_RECORD"
fi
case "${compiler##*/}" in
  rustc | rustc.exe | clippy-driver | clippy-driver.exe) ;;
  *)
    echo "sccache must receive the real compiler after workspace flags are applied" >&2
    exit 1
    ;;
esac
if [[ -n "$HARN_TEST_SCCACHE" ]]; then
  exec "$HARN_TEST_SCCACHE" "$@"
fi
exec "$@"
SH
chmod +x "$probe_bin/sccache"
probe_path="$probe_bin:$PATH"
echo "sccache_rustc_wrapper_test: real Cargo probe sccache $probe_sccache"

output="$tmp_root/cargo-output.txt"
export HARN_TEST_SCCACHE_RECORD="$tmp_root/cache-invocations"
: > "$HARN_TEST_SCCACHE_RECORD"
set +e
PATH="$probe_path" \
  CARGO_TARGET_DIR="$tmp_root/target" \
  RUSTC_WRAPPER="$wrapper_binary" \
  HARN_SCCACHE_WRAPPER_TRACE=1 \
  HARN_ALLOW_RAW_CARGO=1 \
  cargo test --manifest-path "$fixture/Cargo.toml" --no-run -vv \
  > "$output" 2>&1
cargo_status=$?
set -e
if (( cargo_status != 0 )); then
  echo "real Cargo probe failed before the wrapper routes could be verified" >&2
  cat "$output" >&2
  exit "$cargo_status"
fi

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
if [[ ! -s "$HARN_TEST_SCCACHE_RECORD" ]]; then
  echo "ordinary Cargo compilation did not reach the cache invocation recorder" >&2
  exit 1
fi

if [[ "$native_windows" == 0 ]]; then
  cat > "$fixture/src/lib.rs" <<'RS'
pub const WORKSPACE_BOOTSTRAP: &str = env!("RUSTC_BOOTSTRAP");

#[test]
fn workspace_wrapper_reached_compilation() {
    assert_eq!(WORKSPACE_BOOTSTRAP, "1");
}
RS
  workspace_output="$tmp_root/workspace-cargo-output.txt"
  if ! PATH="$probe_path" \
    CARGO_TARGET_DIR="$tmp_root/workspace-target" \
    CARGO_INCREMENTAL=0 \
    RUSTC_WRAPPER="$wrapper_binary" \
    RUSTC_WORKSPACE_WRAPPER="$repo_root/scripts/rustc_frontend_wrapper.sh" \
    HARN_RUSTC_FRONTEND_THREADS=2 \
    HARN_SCCACHE_WRAPPER_TRACE=1 \
    HARN_ALLOW_RAW_CARGO=1 \
    cargo test --manifest-path "$fixture/Cargo.toml" --lib --test binary_env -vv \
    > "$workspace_output" 2>&1; then
    echo "real Cargo probe failed with the workspace and cache wrappers composed" >&2
    cat "$workspace_output" >&2
    exit 1
  fi
  if ! grep -Fq 'workspace_wrapper_reached_compilation ... ok' "$workspace_output"; then
    echo "workspace wrapper did not reach the actual library test" >&2
    cat "$workspace_output" >&2
    exit 1
  fi
  if ! grep -Fq 'cargo_binary_environment_reached_rustc ... ok' "$workspace_output"; then
    echo "workspace wrapper lost the direct Cargo binary environment" >&2
    cat "$workspace_output" >&2
    exit 1
  fi
else
  echo "sccache_rustc_wrapper_test: workspace shell wrapper is disabled on native Windows"
fi

clippy_compositions=(compiler)
if [[ "$native_windows" == 0 ]]; then
  clippy_compositions+=(workspace)
fi
for composition in "${clippy_compositions[@]}"; do
  workspace_wrapper=""
  if [[ "$composition" == workspace ]]; then
    workspace_wrapper="$repo_root/scripts/rustc_frontend_wrapper.sh"
  fi
  for case_name in clean lint; do
    clippy_fixture="$tmp_root/clippy-$composition-$case_name"
    mkdir -p "$clippy_fixture/src"
    printf '[package]\nname = "clippy-wrapper-%s"\nversion = "0.0.0"\nedition = "2021"\n' "$case_name" > "$clippy_fixture/Cargo.toml"
    if [[ "$case_name" == clean ]]; then
      printf 'pub fn ready() -> bool { true }\n' > "$clippy_fixture/src/lib.rs"
    else
      printf 'pub fn ready() -> bool { 1 == 1 }\n' > "$clippy_fixture/src/lib.rs"
    fi
    clippy_output="$tmp_root/clippy-$composition-$case_name-output.txt"
    HARN_TEST_SCCACHE_RECORD="$clippy_fixture/cache-invocations"
    : > "$HARN_TEST_SCCACHE_RECORD"
    clippy_status=0
    if [[ "$composition" == workspace ]]; then
      # Cargo Clippy can replace RUSTC_WORKSPACE_WRAPPER. Exercise the nested
      # compiler contract directly and require the real frontend's scoped flag.
      printf 'const _: [(); 1] = [(); env!("RUSTC_BOOTSTRAP").len()];\n' >> "$clippy_fixture/src/lib.rs"
      PATH="$probe_path" RUSTC_BOOTSTRAP='' HARN_RUSTC_FRONTEND_THREADS=2 HARN_SCCACHE_WRAPPER_TRACE=1 \
        "$wrapper_binary" "$workspace_wrapper" "$(command -v clippy-driver)" "$(command -v rustc)" \
        --edition=2021 --crate-name clippy_workspace_probe --crate-type lib \
        "$clippy_fixture/src/lib.rs" --emit=metadata -o "$clippy_fixture/probe.rmeta" -D warnings \
        > "$clippy_output" 2>&1 || clippy_status=$?
    else
      PATH="$probe_path" CARGO_TARGET_DIR="$tmp_root/clippy-$composition-$case_name-target" \
        RUSTC_WRAPPER="$wrapper_binary" HARN_SCCACHE_WRAPPER_TRACE=1 HARN_ALLOW_RAW_CARGO=1 \
        cargo clippy --offline --manifest-path "$clippy_fixture/Cargo.toml" --lib -- -D warnings \
        > "$clippy_output" 2>&1 || clippy_status=$?
    fi
    if [[ -s "$HARN_TEST_SCCACHE_RECORD" ]]; then
      echo "Clippy entered the compiler cache for $composition/$case_name instead of preserving its direct invocation" >&2
      cat "$HARN_TEST_SCCACHE_RECORD" >&2
      exit 1
    fi
    if [[ "$case_name" == clean && "$clippy_status" != 0 ]] \
      || [[ "$case_name" == lint && "$clippy_status" == 0 ]]; then
      echo "real Clippy returned the wrong verdict for $composition/$case_name: $clippy_status" >&2
      cat "$clippy_output" >&2
      exit 1
    fi
    if [[ "$case_name" == lint ]] && ! grep -Fq 'clippy::eq_op' "$clippy_output"; then
      echo "Clippy failed before reaching the intentional source lint" >&2
      cat "$clippy_output" >&2
      exit 1
    fi
  done
done
echo "sccache_rustc_wrapper_test: real Cargo Clippy accepted clean source and rejected clippy::eq_op for ${clippy_compositions[*]} routes"

echo "sccache_rustc_wrapper_test: ok"
