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
case "$*" in
  'show active-toolchain') echo '1.95.0-x86_64-unknown-linux-gnu (overridden by rust-toolchain.toml)' ;;
  'toolchain list --quiet') printf '%s\n' 'stable-x86_64-unknown-linux-gnu (default)' '1.95.0-x86_64-unknown-linux-gnu (active)' ;;
esac
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

cat > "$tmp_root/bin/uname" <<'SCRIPT'
#!/usr/bin/env bash
if [[ -n "${TOOLCHAIN_TEST_UNAME:-}" ]]; then
  echo "$TOOLCHAIN_TEST_UNAME"
else
  /usr/bin/uname "$@"
fi
SCRIPT
cat > "$tmp_root/bin/sysctl" <<'SCRIPT'
#!/usr/bin/env bash
[[ "${TOOLCHAIN_TEST_CENSUS_FAIL:-0}" != 1 ]] || exit 1
case "$*" in
  '-n hw.logicalcpu') echo "${TOOLCHAIN_TEST_CORES:?}" ;;
  '-n hw.memsize') echo "${TOOLCHAIN_TEST_MEMORY:?}" ;;
  *) exit 2 ;;
esac
SCRIPT
chmod +x "$tmp_root/bin/uname" "$tmp_root/bin/sysctl"

run_case() {
  local name=$1 mode=$2 expected_status=$3
  local state="$tmp_root/$name"
  mkdir -p "$state"
  local status=0
  : > "$state/github_env"
  PATH="$tmp_root/bin:$PATH" \
    GITHUB_ENV="$state/github_env" \
    OS="${CASE_OS:-}" \
    RUNNER_TEMP="$state/runner-temp" \
    HARN_RUSTC_FRONTEND_THREADS="${CASE_FRONTEND_THREADS-8}" \
    RUNNER_ENVIRONMENT="${CASE_RUNNER_ENVIRONMENT:-self-hosted}" \
    TOOLCHAIN_TEST_UNAME="${CASE_UNAME:-}" \
    TOOLCHAIN_TEST_CORES="${CASE_CORES:-}" \
    TOOLCHAIN_TEST_MEMORY="${CASE_MEMORY:-}" \
    TOOLCHAIN_TEST_CENSUS_FAIL="${CASE_CENSUS_FAIL:-0}" \
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

# A hosted runner keeps only the pinned toolchain, so the Rust cache key does
# not depend on which image's floating stable the runner shipped (#9430).
CASE_RUNNER_ENVIRONMENT=github-hosted run_case hosted cargo-transient 0
diff -u - <(grep '^rustup' "$tmp_root/hosted/calls") <<'EXPECTED'
rustup show
rustup component add rustfmt clippy
rustup target add x86_64-pc-windows-msvc
rustup show active-toolchain
rustup default 1.95.0-x86_64-unknown-linux-gnu
rustup toolchain list --quiet
rustup toolchain uninstall stable-x86_64-unknown-linux-gnu
rustup toolchain list --quiet
EXPECTED
# An owned runner's shared rustup home is never touched.
! grep -Eq 'rustup (default|toolchain uninstall)' "$tmp_root/cargo-transient/calls"

# Later steps compile workspace crates through the parallel front-end wrapper,
# and nothing else reaches the job's environment.
# The wrapper is a copy outside the checkout, so switching the checkout to a
# commit without it keeps building.
wrapper_line="$(cat "$tmp_root/rustc-transient/github_env")"
[[ "$wrapper_line" =~ ^RUSTC_WORKSPACE_WRAPPER=$tmp_root/rustc-transient/runner-temp/harn-rustc-frontend/[0-9]+-[0-9]+/rustc_frontend_wrapper\.sh$ ]]
wrapper="${wrapper_line#RUSTC_WORKSPACE_WRAPPER=}"
# A compilation gets the flags and the bootstrap, and keeps Cargo's
# CARGO_BIN_EXE_* variables, whose names are not shell identifiers.
cat > "$tmp_root/fake-rustc" <<'SCRIPT'
#!/usr/bin/env bash
env | grep -E '^(RUSTC_BOOTSTRAP|CARGO_BIN_EXE_echo-env)='
printf 'last-arg %s\n' "${!#}"
SCRIPT
chmod +x "$tmp_root/fake-rustc"
diff -u - <(env 'CARGO_BIN_EXE_echo-env=/bin/echo' 'HARN_RUSTC_FRONTEND_THREADS=8' "$wrapper" "$tmp_root/fake-rustc" --crate-name t | sort) <<'EXPECTED'
CARGO_BIN_EXE_echo-env=/bin/echo
RUSTC_BOOTSTRAP=1
last-arg -Zthreads=8
EXPECTED
# Windows cannot run the shell wrapper, and the opt-out leaves the job serial.
CASE_OS=Windows_NT run_case frontend-windows cargo-transient 0
[[ ! -s "$tmp_root/frontend-windows/github_env" ]]
CASE_FRONTEND_THREADS=1 run_case frontend-off cargo-transient 0
[[ ! -s "$tmp_root/frontend-off/github_env" ]]

# Exercise automatic selection through the real installer, including its
# decision to install a Cargo wrapper or leave workspace compilation serial.
CASE_RUNNER_ENVIRONMENT=github-hosted CASE_UNAME=Darwin CASE_CORES=3 \
  CASE_MEMORY=7516192768 CASE_FRONTEND_THREADS='' \
  run_case frontend-small-mac cargo-transient 0
[[ ! -s "$tmp_root/frontend-small-mac/github_env" ]]
grep -Fq 'rustc front end: serial for workspace crates' "$tmp_root/frontend-small-mac/output"

CASE_RUNNER_ENVIRONMENT=github-hosted CASE_UNAME=Darwin CASE_CORES=5 \
  CASE_MEMORY=15032385536 CASE_FRONTEND_THREADS='' \
  run_case frontend-large-mac cargo-transient 0
grep -Fq 'RUSTC_WORKSPACE_WRAPPER=' "$tmp_root/frontend-large-mac/github_env"
grep -Fq 'rustc front end: -Zthreads=8 for workspace crates' "$tmp_root/frontend-large-mac/output"

CASE_RUNNER_ENVIRONMENT=self-hosted CASE_UNAME=Darwin CASE_CORES=3 \
  CASE_MEMORY=7516192768 CASE_FRONTEND_THREADS='' \
  run_case frontend-owned-mac cargo-transient 0
grep -Fq 'RUSTC_WORKSPACE_WRAPPER=' "$tmp_root/frontend-owned-mac/github_env"

CASE_RUNNER_ENVIRONMENT=github-hosted CASE_UNAME=Linux CASE_FRONTEND_THREADS='' \
  run_case frontend-linux cargo-transient 0
grep -Fq 'RUSTC_WORKSPACE_WRAPPER=' "$tmp_root/frontend-linux/github_env"

CASE_RUNNER_ENVIRONMENT=github-hosted CASE_UNAME=Darwin CASE_CORES=3 \
  CASE_MEMORY=7516192768 CASE_FRONTEND_THREADS=8 \
  run_case frontend-explicit cargo-transient 0
grep -Fq 'RUSTC_WORKSPACE_WRAPPER=' "$tmp_root/frontend-explicit/github_env"

CASE_RUNNER_ENVIRONMENT=github-hosted CASE_UNAME=Darwin CASE_CENSUS_FAIL=1 \
  CASE_FRONTEND_THREADS='' run_case frontend-unmeasured cargo-transient 2
[[ ! -s "$tmp_root/frontend-unmeasured/github_env" ]]
grep -Fq 'cannot measure hosted macOS compiler resources' "$tmp_root/frontend-unmeasured/output"

echo 'rust toolchain action tests passed'
