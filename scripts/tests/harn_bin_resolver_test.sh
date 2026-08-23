#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
real_sleep=$(command -v sleep)
minimum_bash=$(command -v bash)
if [[ -x /bin/bash ]]; then
  # macOS still ships Bash 3.2. Run the production resolver through that
  # supported floor even when Homebrew Bash appears earlier in PATH.
  minimum_bash=/bin/bash
fi
allow_cargo_integration="${HARN_BIN_RESOLVER_TEST_ALLOW_CARGO:-0}"
case "$allow_cargo_integration" in
  0 | 1) ;;
  *)
    echo "HARN_BIN_RESOLVER_TEST_ALLOW_CARGO must be 0 or 1" >&2
    exit 2
    ;;
esac
# Test selection belongs to this harness process, not to Harn or Cargo's typed
# environment contract.
unset HARN_BIN_RESOLVER_TEST_ALLOW_CARGO
# shellcheck source=scripts/lib/cargo_env.sh
source "$repo_root/scripts/lib/cargo_env.sh"
# shellcheck source=scripts/lib/harn_bin.sh
source "$repo_root/scripts/lib/harn_bin.sh"

tmp_root=$(mktemp -d)
cleanup() {
  rm -rf "$tmp_root"
  harn_refresh_cargo_target_dir_cache >/dev/null 2>&1 || true
}
trap cleanup EXIT

fake_bin="$tmp_root/harn"
cat > "$fake_bin" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" = "__internal-freshness-evidence-v5" ]]; then
  if [[ "${FAKE_HARN_OVERSIZED_EVIDENCE_ERROR:-0}" = "1" ]]; then
    for index in 1 2 3 4 5 6; do
      printf 'typed-reason-%s:%02048d\n' "$index" 0 >&2
    done
    exit 1
  fi
  if [[ ! -r "$2" ]]; then
    echo "Cargo dep-info is missing at $2" >&2
    exit 1
  fi
  binary_hash="$(git hash-object --no-filters -- "$3")000000000000000000000000"
  dep_hash="$(git hash-object --no-filters -- "$2")000000000000000000000000"
  if [[ -n "${7:-}" ]]; then
    printf 'harn-freshness-manifest-v3\n' >"$7"
  fi
  printf 'harn-artifact-evidence-v5-cargo-output-dep-info-v1-manifest-3\nbuild-freshness=%s\nbuild-id=%s\nartifact-stat=%s\ndep-info=%s\ndependencies=%s\n' \
    "$(cat "$3.build-freshness" 2>/dev/null || true)" "$binary_hash" \
    "$binary_hash" "$dep_hash" "$dep_hash"
  exit 0
fi
if [[ "${1:-}" = "--print-inherited-harn-bin" ]]; then
  printf '%s\n' "${HARN_BIN-}"
else
  printf 'fake harn\n'
fi
SH
chmod +x "$fake_bin"

HARN_BIN="$fake_bin" "$repo_root/scripts/harn_bin.sh" --print >"$tmp_root/explicit.out"
if ! grep -Fxq "$fake_bin" "$tmp_root/explicit.out"; then
  echo "harn_bin resolver did not return the explicit executable HARN_BIN" >&2
  cat "$tmp_root/explicit.out" >&2
  exit 1
fi
explicit_child_bin="$(HARN_BIN="$fake_bin" \
  "$repo_root/scripts/harn_bin.sh" -- --print-inherited-harn-bin)"
if [[ "$explicit_child_bin" != "$fake_bin" ]]; then
  echo "harn_bin runner did not propagate the explicit binary to its child" >&2
  exit 1
fi

snapshot="$(harn_snapshot_binary "$fake_bin" "$tmp_root/stable/harn-bin")"
if [[ "$snapshot" != "$tmp_root/stable/harn-bin/harn" ]] || [[ ! -x "$snapshot" ]]; then
  echo "harn binary snapshot did not produce the canonical executable path" >&2
  exit 1
fi
rm "$fake_bin"
if [[ "$("$snapshot")" != "fake harn" ]]; then
  echo "harn binary snapshot still depended on the mutable source path" >&2
  exit 1
fi

# A post-build identity failure is a release-gate diagnostic, not a best-effort
# probe. Preserve the typed producer reason and the exact structural input
# states so native CI can distinguish a missing dep-info file from a stale or
# non-executable artifact without logging any dependency contents.
diagnostic_target="$tmp_root/diagnostic target"
mkdir -p "$diagnostic_target/debug"
cp "$snapshot" "$diagnostic_target/debug/harn"
if harn_build_freshness_id "$diagnostic_target/debug/harn" 1 \
  >"$tmp_root/artifact-diagnostic.out" \
  2>"$tmp_root/artifact-diagnostic.err"; then
  echo "post-build freshness identity accepted missing Cargo dep-info" >&2
  exit 1
fi
if ! grep -Fq \
    'binary=regular-readable-executable' "$tmp_root/artifact-diagnostic.err" || \
   ! grep -Fq 'dep-info=missing' "$tmp_root/artifact-diagnostic.err" || \
   ! grep -Fq 'Harn artifact evidence producer: Cargo dep-info is missing at' \
    "$tmp_root/artifact-diagnostic.err"; then
  echo "post-build freshness diagnostic omitted its typed artifact state" >&2
  cat "$tmp_root/artifact-diagnostic.err" >&2
  exit 1
fi
if [[ -s "$tmp_root/artifact-diagnostic.out" ]]; then
  echo "post-build freshness diagnostic leaked evidence on stdout" >&2
  cat "$tmp_root/artifact-diagnostic.out" >&2
  exit 1
fi
if FAKE_HARN_OVERSIZED_EVIDENCE_ERROR=1 \
  harn_build_freshness_id "$diagnostic_target/debug/harn" 1 \
    >"$tmp_root/bounded-diagnostic.out" \
    2>"$tmp_root/bounded-diagnostic.err"; then
  echo "post-build freshness identity accepted a rejected artifact" >&2
  exit 1
fi
if [[ "$(grep -Fc 'Harn artifact evidence producer:' \
      "$tmp_root/bounded-diagnostic.err")" != "4" ]] || \
   [[ "$(wc -c <"$tmp_root/bounded-diagnostic.err")" -gt 4800 ]]; then
  echo "post-build freshness producer diagnostic was not bounded" >&2
  wc -c "$tmp_root/bounded-diagnostic.err" >&2
  exit 1
fi

fake_exe="$tmp_root/harn.exe"
cp "$snapshot" "$fake_exe"
exe_snapshot="$(harn_snapshot_binary "$fake_exe" "$tmp_root/stable/windows")"
if [[ "$exe_snapshot" != "$tmp_root/stable/windows/harn.exe" ]]; then
  echo "harn binary snapshot did not preserve the Windows executable suffix" >&2
  exit 1
fi

non_exec="$tmp_root/not-executable"
printf 'not executable\n' > "$non_exec"
if HARN_BIN="$non_exec" "$repo_root/scripts/harn_bin.sh" --print >"$tmp_root/non-exec.out" 2>"$tmp_root/non-exec.err"; then
  echo "harn_bin resolver accepted a non-executable HARN_BIN" >&2
  cat "$tmp_root/non-exec.out" >&2
  exit 1
fi
if ! grep -Fq "harn binary is not executable" "$tmp_root/non-exec.err"; then
  echo "non-executable HARN_BIN error did not explain the validation failure" >&2
  cat "$tmp_root/non-exec.err" >&2
  exit 1
fi

fake_cargo_bin="$tmp_root/fake-cargo-bin"
target_dir="$tmp_root/target dir"
record="$tmp_root/cargo-record.txt"
mkdir -p "$fake_cargo_bin" "$target_dir/debug"
cat > "$fake_cargo_bin/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
normalized_args="$*"
if [[ "${1:-}" = "--config" && \
      "${2:-}" = env.HARN_BUILD_FRESHNESS_ID=* ]]; then
  build_freshness_id="${2#env.HARN_BUILD_FRESHNESS_ID=}"
  build_freshness_id="${build_freshness_id#\'}"
  build_freshness_id="${build_freshness_id%\'}"
  if [[ ! "$build_freshness_id" =~ ^([0-9a-f]{40}|[0-9a-f]{64})$ ]]; then
    echo "fake Cargo received malformed build freshness config" >&2
    exit 2
  fi
  export HARN_BUILD_FRESHNESS_ID="$build_freshness_id"
  shift 2
  normalized_args="--config env.HARN_BUILD_FRESHNESS_ID='<freshness>' $*"
fi
{
  printf 'args=%s\n' "$normalized_args"
  printf 'CARGO_TARGET_DIR=%s\n' "${CARGO_TARGET_DIR-__unset__}"
  printf 'CARGO_BUILD_BUILD_DIR=%s\n' "${CARGO_BUILD_BUILD_DIR-__unset__}"
  printf 'RUSTC_WRAPPER=%s\n' "${RUSTC_WRAPPER-__unset__}"
  printf 'RUSTC_WORKSPACE_WRAPPER=%s\n' "${RUSTC_WORKSPACE_WRAPPER-__unset__}"
  printf 'CARGO_BUILD_RUSTC_WRAPPER=%s\n' "${CARGO_BUILD_RUSTC_WRAPPER-__unset__}"
  printf 'CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER=%s\n' "${CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER-__unset__}"
  printf 'SCCACHE_DISABLE=%s\n' "${SCCACHE_DISABLE-__unset__}"
  if [[ -n "${HARN_BUILD_FRESHNESS_ID:-}" ]]; then
    printf 'HARN_BUILD_FRESHNESS_ID=<set>\n'
  else
    printf 'HARN_BUILD_FRESHNESS_ID=__unset__\n'
  fi
} >> "$FAKE_CARGO_RECORD"

case "${FAKE_CARGO_MODE:-success}" in
  ordinary-failure)
    echo "ordinary cargo failure" >&2
    exit 17
    ;;
  plain-timeout)
    tail -f /dev/null &
    printf '%s\n' "$!" > "${FAKE_CARGO_CHILD_PID_FILE:?}"
    printf 'ready\n' > "${FAKE_CARGO_TIMEOUT_FIFO:?}"
    wait "$!"
    ;;
  wrapper-timeout|wrapper-timeout-retry-failure)
    if [[ -n "${RUSTC_WRAPPER:-}" || -n "${RUSTC_WORKSPACE_WRAPPER:-}" || \
          -n "${CARGO_BUILD_RUSTC_WRAPPER:-}" || \
          -n "${CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER:-}" ]]; then
      tail -f /dev/null &
      printf '%s\n' "$!" > "${FAKE_CARGO_CHILD_PID_FILE:?}"
      printf 'ready\n' > "${FAKE_CARGO_TIMEOUT_FIFO:?}"
      wait "$!"
    fi
    if [[ "$FAKE_CARGO_MODE" = "wrapper-timeout-retry-failure" ]]; then
      echo "retry cargo failure" >&2
      exit 19
    fi
    ;;
  wrapper-timeout-always)
    tail -f /dev/null &
    printf '%s\n' "$!" >> "${FAKE_CARGO_CHILD_PID_FILE:?}"
    printf 'ready\n' > "${FAKE_CARGO_TIMEOUT_FIFO:?}"
    wait "$!"
    ;;
esac

case "$*" in
  "metadata --format-version=1 --no-deps")
    printf '{"target_directory":"%s"}\n' "${FAKE_METADATA_TARGET_DIR:?}"
    ;;
  "build --quiet --bin harn --bin harn-freshness-check --features internal-freshness-checker")
    mkdir -p "${CARGO_TARGET_DIR:?}/debug"
    cat > "$CARGO_TARGET_DIR/debug/harn" <<'BIN'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" = "host" ]]; then
  exit 2
fi
if [[ "${1:-}" = "__internal-executable-path" ]]; then
  printf '%s\n' "$0"
  exit 0
fi
if [[ "${1:-}" = "__internal-freshness-evidence-v5" ]]; then
  binary_hash="$(git hash-object --no-filters -- "$3")000000000000000000000000"
  dep_hash="$(git hash-object --no-filters -- "$2")000000000000000000000000"
  if [[ -n "${7:-}" ]]; then
    printf 'harn-freshness-manifest-v3\n' >"$7"
  fi
  printf 'harn-artifact-evidence-v5-cargo-output-dep-info-v1-manifest-3\nbuild-freshness=%s\nbuild-id=%s\nartifact-stat=%s\ndep-info=%s\ndependencies=%s\n' \
    "$(cat "$3.build-freshness")" "$binary_hash" "$binary_hash" \
    "$dep_hash" "$dep_hash"
  exit 0
fi
if [[ "${1:-}" = "--print-inherited-harn-bin" ]]; then
  printf '%s\n' "${HARN_BIN-}"
else
  printf 'fake harn\n'
fi
BIN
    chmod +x "$CARGO_TARGET_DIR/debug/harn"
    cat > "$CARGO_TARGET_DIR/debug/harn-freshness-check" <<'CHECKER'
#!/usr/bin/env bash
set -euo pipefail
case "${1:-}" in
  record-evidence)
    printf 'harn-freshness-check-v4\nrepo-path=%064d\nchecker-build-id=aa\nchecker-content=%064d\nmanifest=%064d\n' 0 0 0
    ;;
  verify) exit 0 ;;
  *) exit 2 ;;
esac
CHECKER
    chmod +x "$CARGO_TARGET_DIR/debug/harn-freshness-check"
    printf '%s\n' "${HARN_BUILD_FRESHNESS_ID:?}" \
      > "$CARGO_TARGET_DIR/debug/harn.build-freshness"
    escaped_harn="${CARGO_TARGET_DIR// /\\ }/debug/harn"
    printf '%s:\n' "$escaped_harn" > "$CARGO_TARGET_DIR/debug/harn.d"
    ;;
  *)
    echo "unexpected cargo invocation: $*" >&2
    exit 2
    ;;
esac
SH
chmod +x "$fake_cargo_bin/cargo"

# The production watchdog uses `sleep`, but a sub-second sleep is not a stable
# test clock under runner load. This PATH-scoped adapter rendezvous with the
# fake Cargo child through a FIFO: timeout cases advance only after the child
# has actually blocked, while successful or failing retry probes leave the
# watchdog asleep until the resolver cancels it.
cat > "$fake_cargo_bin/sleep" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

if [[ -z "${FAKE_CARGO_TIMEOUT_FIFO:-}" || "${1:-}" != "0.1" ]]; then
  if [[ -n "${FAKE_CARGO_TIMEOUT_FIFO:-}" && "${1:-}" = "1" ]]; then
    exit 0
  fi
  exec "${FAKE_REAL_SLEEP:?}" "$@"
fi

case "${FAKE_CARGO_MODE:-success}" in
  plain-timeout|wrapper-timeout-always)
    IFS= read -r _ < "$FAKE_CARGO_TIMEOUT_FIFO"
    ;;
  wrapper-timeout|wrapper-timeout-retry-failure)
    if [[ -n "${RUSTC_WRAPPER:-}" || -n "${RUSTC_WORKSPACE_WRAPPER:-}" || \
          -n "${CARGO_BUILD_RUSTC_WRAPPER:-}" || \
          -n "${CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER:-}" ]]; then
      IFS= read -r _ < "$FAKE_CARGO_TIMEOUT_FIFO"
    else
      exec "$FAKE_REAL_SLEEP" 600
    fi
    ;;
  *) exec "$FAKE_REAL_SLEEP" "$@" ;;
esac
SH
chmod +x "$fake_cargo_bin/sleep"
export FAKE_REAL_SLEEP="$real_sleep"

fake_lease_runner="$fake_cargo_bin/harn-lease-runner"
cat > "$fake_lease_runner" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$*" = "host lease run cargo --help" ]]; then
  echo '      --workload-timeout-ms <WORKLOAD_TIMEOUT_MS>'
  exit 0
fi
previous=""
for arg in "$@"; do
  if [[ "$previous" = "--workload-timeout-ms" && -n "${FAKE_CARGO_LEASE_TIMEOUT_RECORD:-}" ]]; then
    printf '%s\n' "$arg" > "$FAKE_CARGO_LEASE_TIMEOUT_RECORD"
  fi
  previous="$arg"
done
while [[ $# -gt 0 && "$1" != "--" ]]; do shift; done
if [[ $# -eq 0 ]]; then
  echo "fake lease runner did not receive a Cargo command" >&2
  exit 2
fi
shift
lock="${FAKE_CARGO_LEASE_LOCK:?}"
while ! mkdir "$lock" 2>/dev/null; do
  : > "${FAKE_CARGO_LEASE_WAITING:?}"
  sleep 0.01
done
cleanup_fake_lease() { rmdir "$lock" 2>/dev/null || true; }
trap cleanup_fake_lease EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
export CARGO_HARN_HOST_LEASE_ACTIVE=1
cargo "$@"
SH
chmod +x "$fake_lease_runner"

# The resolver's explicit binary and build policy are a coupled input. This
# test supplies both independently below, so do not inherit either from the
# caller (notably `make all` with a prebuilt Harn binary).
unset HARN_BIN HARN_BIN_NO_BUILD

fake_lease_lock="$tmp_root/fake-rust-heavy.lock"
fake_lease_waiting="$tmp_root/fake-rust-heavy.waiting"
fake_lease_timeout_record="$tmp_root/fake-rust-heavy.workload-timeout-ms"
mkdir "$fake_lease_lock"
env -u CARGO_TARGET_DIR -u CARGO_BUILD_BUILD_DIR \
  CARGO_TARGET_DIR="$target_dir" \
  FAKE_CARGO_RECORD="$record" \
  FAKE_CARGO_LEASE_LOCK="$fake_lease_lock" \
  FAKE_CARGO_LEASE_WAITING="$fake_lease_waiting" \
  FAKE_CARGO_LEASE_TIMEOUT_RECORD="$fake_lease_timeout_record" \
  HARN_CARGO_LEASE_RUNNER="$fake_lease_runner" \
  HARN_CARGO_LEASE_MODE=required \
  PATH="$fake_cargo_bin:$PATH" \
  "$minimum_bash" "$repo_root/scripts/harn_bin.sh" --print > "$tmp_root/cargo-run.out" &
resolver_pid=$!
for _ in {1..6000}; do
  [[ -e "$fake_lease_waiting" ]] && break
  if [[ -s "$record" ]] || ! kill -0 "$resolver_pid" 2>/dev/null; then
    break
  fi
  "$real_sleep" 0.01
done
if [[ ! -e "$fake_lease_waiting" ]]; then
  rmdir "$fake_lease_lock"
  wait "$resolver_pid" || true
  echo "harn_bin resolver did not wait behind the active Cargo lease" >&2
  exit 1
fi
if [[ -s "$record" ]]; then
  rmdir "$fake_lease_lock"
  wait "$resolver_pid" || true
  echo "harn_bin resolver overlapped an active leased Cargo job" >&2
  cat "$record" >&2
  exit 1
fi
rmdir "$fake_lease_lock"
wait "$resolver_pid"
expected_bin="$target_dir/debug/harn"
if ! grep -Fxq "$expected_bin" "$tmp_root/cargo-run.out"; then
  echo "harn_bin resolver did not return Cargo's executable-path probe result" >&2
  cat "$tmp_root/cargo-run.out" >&2
  exit 1
fi
leased_probe_args="args=--config env.HARN_BUILD_FRESHNESS_ID='<freshness>' build --quiet --bin harn --bin harn-freshness-check --features internal-freshness-checker"
probe_args='args=build --quiet --bin harn --bin harn-freshness-check --features internal-freshness-checker'
if ! grep -Fxq "$leased_probe_args" "$record"; then
  echo "harn_bin resolver did not delegate binary resolution to Cargo" >&2
  cat "$record" >&2
  exit 1
fi
if [[ "$(cat "$fake_lease_timeout_record")" != "600000" ]]; then
  echo "harn_bin resolver did not give the lease runner its post-admission Cargo deadline" >&2
  exit 1
fi
if ! grep -Fxq "CARGO_BUILD_BUILD_DIR=$target_dir" "$record"; then
  echo "harn_bin resolver did not align Cargo build-dir with CARGO_TARGET_DIR" >&2
  cat "$record" >&2
  exit 1
fi

# CI's default lease policy is off. The resolver and Cargo wrapper must agree
# on that default so the build freshness input remains an ordinary process env
# edge owned by harn-cli's build script. Injecting it through Cargo `--config`
# invalidates unrelated package fingerprints and previously forced a second
# native-Windows C/C++ dependency graph after the workspace tests were warm.
ci_target_dir="$tmp_root/ci target"
mkdir -p "$ci_target_dir/debug"
: > "$record"
env -u HARN_CARGO_LEASE_MODE -u HARN_CARGO_LEASE_RUNNER \
  CI=true \
  CARGO_TARGET_DIR="$ci_target_dir" \
  FAKE_CARGO_RECORD="$record" \
  PATH="$fake_cargo_bin:$PATH" \
  "$minimum_bash" "$repo_root/scripts/harn_bin.sh" --print > "$tmp_root/ci-cargo-run.out"
if grep -Fq 'args=--config env.HARN_BUILD_FRESHNESS_ID=' "$record"; then
  echo "CI-off resolver transported freshness through global Cargo config" >&2
  cat "$record" >&2
  exit 1
fi
if ! grep -Fxq "$probe_args" "$record" || \
   ! grep -Fxq 'HARN_BUILD_FRESHNESS_ID=<set>' "$record"; then
  echo "CI-off resolver did not preserve the scoped process freshness input" >&2
  cat "$record" >&2
  exit 1
fi
if [[ "$(cat "$tmp_root/ci-cargo-run.out")" != "$ci_target_dir/debug/harn" ]]; then
  echo "CI-off resolver did not return the canonical built artifact" >&2
  exit 1
fi
# The fake lease runner above owns the integration assertion. Keep the
# remaining timeout/policy probes hermetic: they test the resolver watchdog,
# not the machine's real shared lease queue.
export HARN_CARGO_LEASE_MODE=off

auto_child_bin="$(CARGO_TARGET_DIR="$target_dir" \
  FAKE_CARGO_RECORD="$record" \
  PATH="$fake_cargo_bin:$PATH" \
  "$repo_root/scripts/harn_bin.sh" --no-build -- --print-inherited-harn-bin)"
if [[ "$auto_child_bin" != "$expected_bin" ]]; then
  echo "harn_bin runner did not propagate the auto-resolved binary to its child" >&2
  exit 1
fi

: > "$record"
CARGO_TARGET_DIR="$target_dir" \
  FAKE_CARGO_RECORD="$record" \
  PATH="$fake_cargo_bin:$PATH" \
  "$repo_root/scripts/harn_bin.sh" --no-build --print > "$tmp_root/no-build.out"
if ! grep -Fxq "$expected_bin" "$tmp_root/no-build.out"; then
  echo "harn_bin --no-build did not return the target-dir executable" >&2
  cat "$tmp_root/no-build.out" >&2
  exit 1
fi
if [[ -s "$record" ]]; then
  echo "harn_bin --no-build invoked cargo" >&2
  cat "$record" >&2
  exit 1
fi

: > "$record"
# The preceding explicit CARGO_TARGET_DIR probe must not inherit this
# worktree's production target-dir cache into the independent metadata
# discovery case. A missing cache is the boundary this case owns; cleanup
# restores the canonical cache after the suite.
rm -f "$(harn_target_dir_cache_path)"
env -u CARGO_TARGET_DIR -u CARGO_BUILD_BUILD_DIR \
  FAKE_CARGO_RECORD="$record" \
  FAKE_METADATA_TARGET_DIR="$target_dir" \
  PATH="$fake_cargo_bin:$PATH" \
  "$repo_root/scripts/harn_bin.sh" --no-build --print > "$tmp_root/no-build-metadata.out"
if ! grep -Fxq "$expected_bin" "$tmp_root/no-build-metadata.out"; then
  echo "harn_bin --no-build did not discover Cargo's configured target directory" >&2
  cat "$tmp_root/no-build-metadata.out" >&2
  exit 1
fi
if ! grep -Fxq "args=metadata --format-version=1 --no-deps" "$record"; then
  echo "harn_bin --no-build did not use buildless Cargo metadata discovery" >&2
  cat "$record" >&2
  exit 1
fi
if grep -Fq "args=build " "$record"; then
  echo "harn_bin --no-build attempted to build after metadata discovery" >&2
  cat "$record" >&2
  exit 1
fi

: > "$record"
CARGO_TARGET_DIR="$target_dir" \
  FAKE_CARGO_RECORD="$record" \
  HARN_BIN_NO_BUILD=1 \
  PATH="$fake_cargo_bin:$PATH" \
  "$repo_root/scripts/harn_bin.sh" --print > "$tmp_root/env-no-build.out"
if ! grep -Fxq "$expected_bin" "$tmp_root/env-no-build.out"; then
  echo "HARN_BIN_NO_BUILD did not return the target-dir executable" >&2
  cat "$tmp_root/env-no-build.out" >&2
  exit 1
fi
if [[ -s "$record" ]]; then
  echo "HARN_BIN_NO_BUILD invoked cargo" >&2
  cat "$record" >&2
  exit 1
fi

# NUL-delimited authority entries are data, not MSYS command-line arguments.
# The shell must serialize native paths for the Rust consumer on Windows while
# retaining POSIX paths for its own existence checks.
authority_repo="$tmp_root/windows authority repo"
authority_bin="$tmp_root/windows-authority-bin"
authority_list="$tmp_root/windows-authorities"
mkdir -p "$authority_repo/.cargo" "$authority_bin"
git -C "$authority_repo" init -q
printf '[build]\n' > "$authority_repo/.cargo/config.toml"
cat > "$authority_bin/cygpath" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$#" -ne 2 || "$1" != "-w" || "$2" != /* ]]; then
  echo "unexpected authority cygpath invocation: $*" >&2
  exit 2
fi
path="${2//\//\\}"
printf 'C:%s\n' "$path"
SH
chmod +x "$authority_bin/cygpath"
(
  harn_repo_root() { printf '%s\n' "$authority_repo"; }
  OS=Windows_NT PATH="$authority_bin:$PATH" \
    harn_write_freshness_authority_list "$authority_list"
)
seen_cargo_authority=0
while IFS= read -r -d '' authority; do
  if [[ "$authority" != C:\\* ]]; then
    echo "Windows manifest authority was not projected to a native path: $authority" >&2
    exit 1
  fi
  [[ "$authority" == *".cargo" ]] && seen_cargo_authority=1
done < "$authority_list"
if [[ "$seen_cargo_authority" != 1 ]]; then
  echo "Windows manifest authority projection omitted the existing .cargo directory" >&2
  exit 1
fi

# Canonical falsifier: let Cargo build a tiny CLI that embeds tracked and
# ignored .harn inputs, then use the production resolver and Cargo's real
# top-level dep-info. The target and source paths contain spaces so the typed
# Cargo-output adapter, rather than shell tokenization, owns Cargo's
# spaces-only escaping and native Windows path dialect.
if [[ "$allow_cargo_integration" == "1" ]]; then
# The production lease-overlap invariant is exercised above with the explicit
# fake lease runner. This tiny freshness fixture intentionally is not a full
# Harn (`harn host` exits 2), so keep its Cargo builds hermetic instead of
# silently borrowing an ambient installed Harn as a lease runner. Clean CI has
# no such ambient executable; developer machines commonly do.
export HARN_CARGO_LEASE_MODE=off
fixture_lease_runner_marker="$tmp_root/cargo-fixture-lease-runner-invoked"
fixture_lease_runner="$tmp_root/cargo-fixture-unsupported-lease-runner"
cat > "$fixture_lease_runner" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
: > "${FIXTURE_LEASE_RUNNER_MARKER:?}"
exit 97
SH
chmod +x "$fixture_lease_runner"
export FIXTURE_LEASE_RUNNER_MARKER="$fixture_lease_runner_marker"
export HARN_CARGO_LEASE_RUNNER="$fixture_lease_runner"
cargo_fixture="$tmp_root/cargo fixture with spaces"
cargo_target="$cargo_fixture/build output with spaces"
mkdir -p "$cargo_fixture/src"
cat > "$cargo_fixture/Cargo.toml" <<'TOML'
[package]
name = "harn"
version = "0.0.0"
edition = "2021"

[features]
internal-freshness-checker = []

[dependencies]
blake3 = "1.8.7"
buildid = "=1.0.5"
hex = "0.4"

[target.'cfg(windows)'.dependencies]
windows-sys = { version = "0.61.2", features = [
    "Win32_Foundation",
    "Win32_Storage_FileSystem",
] }
TOML
mkdir -p "$cargo_fixture/src/bin" "$cargo_fixture/src/bootstrap"
cp "$repo_root/crates/harn-cli/src/bin/harn-freshness-check.rs" \
  "$cargo_fixture/src/bin/harn-freshness-check.rs"
cp "$repo_root/crates/harn-cli/src/bootstrap/freshness_manifest.rs" \
  "$cargo_fixture/src/bootstrap/freshness_manifest.rs"
cp "$repo_root/crates/harn-cli/src/path_policy.rs" \
  "$cargo_fixture/src/path_policy.rs"
cat > "$cargo_fixture/src/main.rs" <<'RS'
#![allow(dead_code)]

#[path = "bootstrap/freshness_manifest.rs"]
mod freshness_manifest;
mod path_policy;

const TRACKED: &str = include_str!("../embedded tracked.harn");
const IGNORED: &str = include_str!("../embedded ignored.harn");
const BUILD_FRESHNESS: &str = match option_env!("HARN_BUILD_FRESHNESS_ID") {
    Some(value) => value,
    None => "",
};

fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    if args.get(1).map(String::as_str) == Some("host") {
        std::process::exit(2);
    } else if args.get(1).map(String::as_str) == Some("__internal-executable-path") {
        println!("{}", std::env::current_exe().unwrap().display());
    } else if args.get(1).map(String::as_str) == Some("__fixture-build-freshness-id") {
        println!("{BUILD_FRESHNESS}");
    } else if args.get(1).map(String::as_str) == Some("__internal-freshness-evidence-v5") {
        use std::{collections::{BTreeSet, hash_map::DefaultHasher}, fs, hash::{Hash, Hasher}, path::{Path, PathBuf}};
        fn digest(paths: &[&str]) -> String {
            let mut hasher = DefaultHasher::new();
            for path in paths { fs::read(path).unwrap().hash(&mut hasher); }
            format!("{0:016x}{0:016x}{0:016x}{0:016x}", hasher.finish())
        }
        fn covered_paths(path: &str) -> BTreeSet<PathBuf> {
            fs::read(path).unwrap().split(|byte| *byte == 0)
                .filter(|value| !value.is_empty())
                .map(|value| PathBuf::from(String::from_utf8(value.to_vec()).unwrap()))
                .collect()
        }
        if args.len() == 8 {
            let repo = Path::new(&args[4]);
            let dependencies = [
                (repo.join("Cargo.toml"), true),
                (repo.join("src/main.rs"), true),
                (repo.join("embedded tracked.harn"), true),
                (repo.join("embedded ignored.harn"), false),
            ];
            freshness_manifest::write_manifest(
                Path::new(&args[7]), repo, &covered_paths(&args[5]),
                Path::new(&args[2]), &dependencies, Path::new(&args[6]),
            ).unwrap();
        }
        println!("harn-artifact-evidence-v5-cargo-output-dep-info-v1-manifest-3");
        println!("build-freshness={BUILD_FRESHNESS}");
        println!("build-id={}", digest(&[&args[3]]));
        println!("artifact-stat={}", freshness_manifest::artifact_stat_id(Path::new(&args[3])).unwrap());
        println!("dep-info={}", digest(&[&args[2]]));
        println!("dependencies={}", digest(&[
            "Cargo.toml", "src/main.rs", "embedded tracked.harn", "embedded ignored.harn"
        ]));
    } else {
        println!("{}:{}", TRACKED, IGNORED);
    }
}
RS
printf 'tracked-v1\n' > "$cargo_fixture/embedded tracked.harn"
printf 'ignored-v1\n' > "$cargo_fixture/embedded ignored.harn"
cat > "$cargo_fixture/.gitignore" <<'EOF'
/build output with spaces/
/embedded ignored.harn
EOF
printf '*.harn diff=hostile\n' > "$cargo_fixture/.gitattributes"
hostile_diff_marker="$tmp_root/hostile-diff-ran"
hostile_diff="$tmp_root/hostile-diff"
cat > "$hostile_diff" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
: > "${FRESHNESS_TEST_DIFF_MARKER:?}"
exit 91
SH
chmod +x "$hostile_diff"
export FRESHNESS_TEST_DIFF_MARKER="$hostile_diff_marker"
git -C "$cargo_fixture" init -q
git -C "$cargo_fixture" config user.name 'Harn Resolver Test'
git -C "$cargo_fixture" config user.email 'harn-resolver-test@example.invalid'
git -C "$cargo_fixture" config commit.gpgsign false
git -C "$cargo_fixture" config diff.hostile.command "$hostile_diff"
git -C "$cargo_fixture" config diff.hostile.textconv "$hostile_diff"
git -C "$cargo_fixture" add Cargo.toml src/main.rs 'embedded tracked.harn' \
  .gitignore .gitattributes
git -C "$cargo_fixture" commit -qm 'fixture'
# Establish old timestamps before the first build. Later edits preserve both
# size and these exact mtimes, so producer provenance and ignored-dependency
# recovery cannot pass merely because Git or Cargo notices timestamp recency.
touch -t 200001010000 "$cargo_fixture/embedded tracked.harn" \
  "$cargo_fixture/embedded ignored.harn"

(
  cd "$cargo_fixture"
  CARGO_TARGET_DIR="$cargo_target" "$repo_root/scripts/harn_bin.sh" --print \
    > "$tmp_root/cargo-fixture-build.out"
)
cargo_fixture_bin="$cargo_target/debug/harn"
if ! grep -Fxq "$cargo_fixture_bin" "$tmp_root/cargo-fixture-build.out"; then
  echo "harn_bin did not resolve the Cargo fixture binary" >&2
  cat "$tmp_root/cargo-fixture-build.out" >&2
  exit 1
fi
if [[ ! -r "$cargo_fixture_bin.d" ]] || \
   [[ ! -r "$cargo_fixture_bin.freshness" ]] || \
   [[ ! -r "$cargo_fixture_bin.freshness.manifest" ]] || \
   [[ ! -x "$(harn_binary_freshness_checker_path "$cargo_fixture_bin")" ]]; then
  echo "build-mode resolution did not produce Cargo dep-info and exact freshness evidence" >&2
  exit 1
fi
if grep -Eq 'tracked-v1|ignored-v1' \
  "$cargo_fixture_bin.freshness" "$cargo_fixture_bin.freshness.manifest"; then
  echo "freshness receipt leaked dependency content instead of content identities" >&2
  cat "$cargo_fixture_bin.freshness" >&2
  exit 1
fi

no_cargo_bin="$tmp_root/no-cargo-bin"
mkdir -p "$no_cargo_bin"
cat > "$no_cargo_bin/cargo" <<'SH'
#!/usr/bin/env bash
echo "no-build freshness verification invoked Cargo" >&2
exit 97
SH
chmod +x "$no_cargo_bin/cargo"
binary_mtime_seconds() {
  local value=""
  if value="$(stat -c '%Y' "$1" 2>/dev/null)"; then
    printf '%s\n' "$value"
  elif value="$(stat -f '%m' "$1" 2>/dev/null)"; then
    printf '%s\n' "$value"
  else
    echo "could not read binary modification time: $1" >&2
    return 1
  fi
}

fixture_binary_mtime_before="$(binary_mtime_seconds "$cargo_fixture_bin")"
for reuse_round in 1 2; do
  (
    cd "$cargo_fixture"
    CARGO_TARGET_DIR="$cargo_target" PATH="$no_cargo_bin:$PATH" \
      "$repo_root/scripts/harn_bin.sh" --no-build --print \
      > "$tmp_root/cargo-fixture-reuse-$reuse_round.out"
  )
  if ! grep -Fxq "$cargo_fixture_bin" "$tmp_root/cargo-fixture-reuse-$reuse_round.out"; then
    echo "unchanged no-build reuse did not return the Cargo fixture binary" >&2
    exit 1
  fi
done
fixture_binary_mtime_after="$(binary_mtime_seconds "$cargo_fixture_bin")"
if [[ "$fixture_binary_mtime_before" != "$fixture_binary_mtime_after" ]]; then
  echo "unchanged no-build reuse modified the binary" >&2
  exit 1
fi
cp -p "$cargo_fixture_bin" "$tmp_root/cargo-fixture-source-v1-bin"

# Cargo owns its top-level checker output and may replace it while compiling
# later test targets. The receipt binds the producer's immutable checker
# snapshot instead, so ordinary Cargo checker churn cannot invalidate Harn.
cargo_fixture_cargo_checker="$(harn_cargo_freshness_checker_path "$cargo_fixture_bin")"
cargo_fixture_proof_checker="$(harn_binary_freshness_checker_path "$cargo_fixture_bin")"
printf '\nlegitimate-cargo-relink\n' >> "$cargo_fixture_cargo_checker"
(
  cd "$cargo_fixture"
  CARGO_TARGET_DIR="$cargo_target" PATH="$no_cargo_bin:$PATH" \
    "$repo_root/scripts/harn_bin.sh" --no-build --print \
    > "$tmp_root/cargo-checker-churn.out"
)

# A tracked content edit remains stale even when its mtime is forced older than
# the executable. This is the blind spot of a timestamp-only depfile query and
# the reason the receipt composes Git content identity with Cargo recency.
printf 'tracked-v2\n' > "$cargo_fixture/embedded tracked.harn"
touch -t 200001010000 "$cargo_fixture/embedded tracked.harn"
if (
  cd "$cargo_fixture"
  CARGO_TARGET_DIR="$cargo_target" PATH="$no_cargo_bin:$PATH" \
    "$repo_root/scripts/harn_bin.sh" --no-build --print \
    > "$tmp_root/cargo-fixture-tracked-stale.out" \
    2> "$tmp_root/cargo-fixture-tracked-stale.err"
); then
  echo "no-build accepted an older-mtime tracked embedded .harn edit" >&2
  exit 1
fi
if ! grep -Fq 'manifest input content changed' "$tmp_root/cargo-fixture-tracked-stale.err"; then
  echo "tracked content-stale error did not identify the receipt proof" >&2
  cat "$tmp_root/cargo-fixture-tracked-stale.err" >&2
  exit 1
fi
if [[ -e "$hostile_diff_marker" ]]; then
  echo "Git fingerprint executed a configured external diff or textconv driver" >&2
  exit 1
fi

# Restore exact content with an old mtime: the content-addressed proofs accept
# the original build again without compiling.
printf 'tracked-v1\n' > "$cargo_fixture/embedded tracked.harn"
touch -t 200001010000 "$cargo_fixture/embedded tracked.harn"
(
  cd "$cargo_fixture"
  CARGO_TARGET_DIR="$cargo_target" PATH="$no_cargo_bin:$PATH" \
    "$repo_root/scripts/harn_bin.sh" --no-build --print \
    > "$tmp_root/cargo-fixture-restored.out"
)

# A binary compiled from another exact source fingerprint cannot masquerade as
# the current artifact, even when its timestamp is copied from the current
# build. Compiled provenance owns this semantic identity; the platform build
# ID and artifact-stat tuple provide independent accidental-replacement proof.
printf 'tracked-v2\n' > "$cargo_fixture/embedded tracked.harn"
touch -t 200001010000 "$cargo_fixture/embedded tracked.harn"
(
  cd "$cargo_fixture"
  CARGO_TARGET_DIR="$cargo_target" "$repo_root/scripts/harn_bin.sh" --print \
    > "$tmp_root/cargo-fixture-source-v2-build.out"
)
cp -p "$cargo_fixture_bin" "$tmp_root/cargo-fixture-source-v2-bin"
source_v1_freshness_id="$(
  "$tmp_root/cargo-fixture-source-v1-bin" __fixture-build-freshness-id
)"
source_v2_freshness_id="$(
  "$tmp_root/cargo-fixture-source-v2-bin" __fixture-build-freshness-id
)"
if [[ ! "$source_v1_freshness_id" =~ ^([0-9a-f]{40}|[0-9a-f]{64})$ ]] || \
   [[ ! "$source_v2_freshness_id" =~ ^([0-9a-f]{40}|[0-9a-f]{64})$ ]] || \
   [[ "$source_v1_freshness_id" = "$source_v2_freshness_id" ]]; then
  echo "compiled provenance did not distinguish exact source fingerprints" >&2
  exit 1
fi
cp "$tmp_root/cargo-fixture-source-v1-bin" "$cargo_fixture_bin"
touch -r "$tmp_root/cargo-fixture-source-v2-bin" "$cargo_fixture_bin"
if (
  cd "$cargo_fixture"
  CARGO_TARGET_DIR="$cargo_target" PATH="$no_cargo_bin:$PATH" \
    "$repo_root/scripts/harn_bin.sh" --no-build --print \
    > "$tmp_root/cargo-fixture-foreign-source-bin.out" \
    2> "$tmp_root/cargo-fixture-foreign-source-bin.err"
); then
  echo "no-build accepted a same-mtime binary from another source fingerprint" >&2
  exit 1
fi
if ! grep -Fq 'worktree Harn executable changed' \
  "$tmp_root/cargo-fixture-foreign-source-bin.err"; then
  echo "foreign-source binary failure did not identify artifact evidence" >&2
  cat "$tmp_root/cargo-fixture-foreign-source-bin.err" >&2
  exit 1
fi

# Restore the original source state through the supported build path so the
# subsequent byte-mutation falsifier starts from a valid receipt.
printf 'tracked-v1\n' > "$cargo_fixture/embedded tracked.harn"
touch -t 200001010000 "$cargo_fixture/embedded tracked.harn"
(
  cd "$cargo_fixture"
  CARGO_TARGET_DIR="$cargo_target" "$repo_root/scripts/harn_bin.sh" --print \
    > "$tmp_root/cargo-fixture-source-v1-rebuild.out"
)

# The auto-resolved worktree path binds ordinary filesystem identity as well as
# semantic provenance. A byte-identical copy remains a valid caller-owned
# explicit pin, but replacing the canonical artifact with that copy is
# unproven and must fail until Cargo relinks it.
cp -p "$cargo_fixture_bin" "$tmp_root/cargo-fixture-identical-copy"
explicit_copy="$(HARN_BIN="$tmp_root/cargo-fixture-identical-copy" \
  "$repo_root/scripts/harn_bin.sh" --print)"
if [[ "$explicit_copy" != "$tmp_root/cargo-fixture-identical-copy" ]]; then
  echo "explicit HARN_BIN did not retain caller-owned byte-identical copy semantics" >&2
  exit 1
fi
rm "$cargo_fixture_bin"
cp "$tmp_root/cargo-fixture-identical-copy" "$cargo_fixture_bin"
touch -r "$tmp_root/cargo-fixture-identical-copy" "$cargo_fixture_bin"
if (
  cd "$cargo_fixture"
  CARGO_TARGET_DIR="$cargo_target" PATH="$no_cargo_bin:$PATH" \
    "$repo_root/scripts/harn_bin.sh" --no-build --print \
    > "$tmp_root/cargo-fixture-identical-replacement.out" \
    2> "$tmp_root/cargo-fixture-identical-replacement.err"
); then
  echo "no-build accepted a byte-identical replacement at the canonical path" >&2
  exit 1
fi
(
  cd "$cargo_fixture"
  CARGO_TARGET_DIR="$cargo_target" "$repo_root/scripts/harn_bin.sh" --print \
    > "$tmp_root/cargo-fixture-identical-recovered.out"
)

# Replacing the artifact bytes without changing its timestamp must fail even
# when every source and dependency remains unchanged.
cp -p "$cargo_fixture_bin" "$tmp_root/cargo-fixture-bin.saved"
printf '\nreplacement-bytes\n' >> "$cargo_fixture_bin"
touch -r "$tmp_root/cargo-fixture-bin.saved" "$cargo_fixture_bin"
if (
  cd "$cargo_fixture"
  CARGO_TARGET_DIR="$cargo_target" PATH="$no_cargo_bin:$PATH" \
    "$repo_root/scripts/harn_bin.sh" --no-build --print \
    > "$tmp_root/cargo-fixture-binary-stale.out" \
    2> "$tmp_root/cargo-fixture-binary-stale.err"
); then
  echo "no-build accepted a same-mtime replacement Harn binary" >&2
  exit 1
fi
if ! grep -Fq 'worktree Harn executable changed' \
  "$tmp_root/cargo-fixture-binary-stale.err"; then
  echo "binary replacement failure did not identify artifact evidence" >&2
  cat "$tmp_root/cargo-fixture-binary-stale.err" >&2
  exit 1
fi
cp "$tmp_root/cargo-fixture-bin.saved" "$cargo_fixture_bin"

# Build mode must not merely bless the replaced artifact. It removes an
# unproven canonical output and makes Cargo relink it before publishing a new
# receipt, even when Cargo's own fingerprint would otherwise report fresh.
(
  cd "$cargo_fixture"
  CARGO_TARGET_DIR="$cargo_target" "$repo_root/scripts/harn_bin.sh" --print \
    > "$tmp_root/cargo-fixture-recovered-binary.out"
)
if [[ "$("$cargo_fixture_bin")" != $'tracked-v1\n:ignored-v1' ]]; then
  echo "build-mode recovery did not restore the exact embedded fixture inputs" >&2
  exit 1
fi

# Ignored/generated or external dependencies are outside Git ownership. Exact
# Cargo prerequisite hashing catches their edits even when mtimes roll back.
printf 'ignored-v2\n' > "$cargo_fixture/embedded ignored.harn"
touch -t 200001010000 "$cargo_fixture/embedded ignored.harn"
if (
  cd "$cargo_fixture"
  CARGO_TARGET_DIR="$cargo_target" PATH="$no_cargo_bin:$PATH" \
    "$repo_root/scripts/harn_bin.sh" --no-build --print \
    > "$tmp_root/cargo-fixture-ignored-stale.out" \
    2> "$tmp_root/cargo-fixture-ignored-stale.err"
); then
  echo "no-build accepted an older-mtime ignored embedded .harn dependency" >&2
  exit 1
fi
if ! grep -Fq 'manifest input content changed' "$tmp_root/cargo-fixture-ignored-stale.err"; then
  echo "ignored dependency-stale error did not identify exact artifact evidence" >&2
  cat "$tmp_root/cargo-fixture-ignored-stale.err" >&2
  exit 1
fi

# The supported recovery must also defeat Cargo's mtime-only blind spot: an
# invalid receipt causes the exact output to be relinked with current compiled
# provenance instead of allowing a stale executable to be re-certified.
(
  cd "$cargo_fixture"
  CARGO_TARGET_DIR="$cargo_target" "$repo_root/scripts/harn_bin.sh" --print \
    > "$tmp_root/cargo-fixture-ignored-rebuild.out"
)
if [[ "$("$cargo_fixture_bin")" != $'tracked-v1\n:ignored-v2' ]]; then
  echo "build mode did not relink an older-mtime ignored dependency edit" >&2
  exit 1
fi

# Return the canonical fixture to its original exact state for fail-closed
# missing/invalid dependency-evidence probes below.
printf 'ignored-v1\n' > "$cargo_fixture/embedded ignored.harn"
touch -t 200001010000 "$cargo_fixture/embedded ignored.harn"
(
  cd "$cargo_fixture"
  CARGO_TARGET_DIR="$cargo_target" "$repo_root/scripts/harn_bin.sh" --print \
    > "$tmp_root/cargo-fixture-ignored-restored.out"
)

# Missing and malformed receipt evidence must fail closed. These use a
# separate executable so the canonical fixture receipt remains attributable;
# dep-info corruption below is exercised against the canonical fixture.
unproven_target="$tmp_root/unproven target"
mkdir -p "$unproven_target/debug"
cp "$snapshot" "$unproven_target/debug/harn"
escaped_unproven="${unproven_target// /\\ }/debug/harn"
printf '%s:\n' "$escaped_unproven" > "$unproven_target/debug/harn.d"
if CARGO_TARGET_DIR="$unproven_target" PATH="$no_cargo_bin:$PATH" \
  "$repo_root/scripts/harn_bin.sh" --no-build --print \
  > "$tmp_root/missing-receipt.out" 2> "$tmp_root/missing-receipt.err"; then
  echo "no-build accepted an executable without a build receipt" >&2
  exit 1
fi
if ! grep -Fq 'build receipt is missing' "$tmp_root/missing-receipt.err"; then
  echo "missing-receipt failure was not attributable" >&2
  cat "$tmp_root/missing-receipt.err" >&2
  exit 1
fi

printf 'not-a-receipt\n' > "$unproven_target/debug/harn.freshness"
cp "$cargo_fixture_bin.freshness.manifest" \
  "$unproven_target/debug/harn.freshness.manifest"
unproven_checker="$(harn_binary_freshness_checker_path \
  "$unproven_target/debug/harn")"
cp "$cargo_fixture_proof_checker" "$unproven_checker"
if CARGO_TARGET_DIR="$unproven_target" PATH="$no_cargo_bin:$PATH" \
  "$repo_root/scripts/harn_bin.sh" --no-build --print \
  > "$tmp_root/malformed-receipt.out" 2> "$tmp_root/malformed-receipt.err"; then
  echo "no-build accepted a malformed build receipt" >&2
  exit 1
fi
if ! grep -Fq 'malformed Harn freshness receipt' "$tmp_root/malformed-receipt.err"; then
  echo "malformed-receipt failure was not attributable" >&2
  cat "$tmp_root/malformed-receipt.err" >&2
  exit 1
fi

mv "$cargo_fixture_bin.d" "$cargo_fixture_bin.d.saved"
if (
  cd "$cargo_fixture"
  CARGO_TARGET_DIR="$cargo_target" PATH="$no_cargo_bin:$PATH" \
  "$repo_root/scripts/harn_bin.sh" --no-build --print \
    > "$tmp_root/missing-dep-info.out" 2> "$tmp_root/missing-dep-info.err"
); then
  echo "no-build accepted an executable without Cargo dep-info" >&2
  exit 1
fi
if ! grep -Eq 'cannot inspect manifest input|manifest input type changed' \
  "$tmp_root/missing-dep-info.err"; then
  echo "missing-dep-info failure was not attributable" >&2
  cat "$tmp_root/missing-dep-info.err" >&2
  exit 1
fi
mv "$cargo_fixture_bin.d.saved" "$cargo_fixture_bin.d"

mv "$cargo_fixture_bin.freshness.manifest" \
  "$cargo_fixture_bin.freshness.manifest.saved"
if (
  cd "$cargo_fixture"
  CARGO_TARGET_DIR="$cargo_target" PATH="$no_cargo_bin:$PATH" \
    "$repo_root/scripts/harn_bin.sh" --no-build --print \
    > "$tmp_root/missing-manifest.out" 2> "$tmp_root/missing-manifest.err"
); then
  echo "no-build accepted an executable without its exact input manifest" >&2
  exit 1
fi
if ! grep -Fq 'input manifest is missing' "$tmp_root/missing-manifest.err"; then
  echo "missing-manifest failure was not attributable" >&2
  cat "$tmp_root/missing-manifest.err" >&2
  exit 1
fi
mv "$cargo_fixture_bin.freshness.manifest.saved" \
  "$cargo_fixture_bin.freshness.manifest"

cp "$cargo_fixture_bin.d" "$cargo_fixture_bin.d.saved"
printf 'this is not make syntax\n' > "$cargo_fixture_bin.d"
if (
  cd "$cargo_fixture"
  CARGO_TARGET_DIR="$cargo_target" PATH="$no_cargo_bin:$PATH" \
  "$repo_root/scripts/harn_bin.sh" --no-build --print \
    > "$tmp_root/malformed-dep-info.out" 2> "$tmp_root/malformed-dep-info.err"
); then
  echo "no-build accepted malformed Cargo dep-info" >&2
  exit 1
fi
if ! grep -Fq 'manifest input content changed' \
  "$tmp_root/malformed-dep-info.err"; then
  echo "malformed-dep-info failure was not attributable" >&2
  cat "$tmp_root/malformed-dep-info.err" >&2
  exit 1
fi
mv "$cargo_fixture_bin.d.saved" "$cargo_fixture_bin.d"

# Windows auto-resolution uses harn.exe but Cargo still names the top-level
# dep-info harn.d. Exercise that suffix contract with Git-Bash path semantics.
windows_target="$tmp_root/windows target"
mkdir -p "$windows_target/debug"
cp "$fake_exe" "$windows_target/debug/harn.exe"
escaped_windows="${windows_target// /\\ }/debug/harn.exe"
printf '%s:\n' "$escaped_windows" > "$windows_target/debug/harn.d"
if [[ "$(OS=Windows_NT harn_binary_dep_info_path "$windows_target/debug/harn.exe")" != \
      "$windows_target/debug/harn.d" ]]; then
  echo "Windows harn.exe resolution did not preserve Cargo's harn.d contract" >&2
  exit 1
fi
if [[ "$(harn_binary_freshness_checker_path "$windows_target/debug/harn.exe")" != \
      "$windows_target/debug/harn.freshness-check.exe" ]]; then
  echo "Windows proof checker snapshot did not preserve an executable suffix" >&2
  exit 1
fi

# Canonical producers must refresh the worktree receipt even when their parent
# process carries an unrelated exact binary pin or no-build policy.
make -C "$repo_root" -n build-harn HARN_BIN="$snapshot" HARN_BIN_NO_BUILD=1 \
  > "$tmp_root/build-harn-dry-run.out"
if ! grep -Fq "HARN_BIN='' HARN_BIN_NO_BUILD=0 ./scripts/harn_bin.sh --print" \
  "$tmp_root/build-harn-dry-run.out"; then
  echo "build-harn did not clear inherited resolver policy before recording freshness" >&2
  cat "$tmp_root/build-harn-dry-run.out" >&2
  exit 1
fi
if [[ "$(grep -nE 'harn_bin.sh --print|sign_local_macos|record-receipt' \
      "$tmp_root/build-harn-dry-run.out" | cut -d: -f2- | sed -n '1p')" != *'harn_bin.sh --print'* ]] || \
   [[ "$(grep -nE 'harn_bin.sh --print|sign_local_macos|record-receipt' \
      "$tmp_root/build-harn-dry-run.out" | cut -d: -f2- | sed -n '2p')" != *'sign_local_macos'* ]] || \
   [[ "$(grep -nE 'harn_bin.sh --print|sign_local_macos|record-receipt' \
      "$tmp_root/build-harn-dry-run.out" | cut -d: -f2- | sed -n '3p')" != *'record-receipt'* ]]; then
  echo "build-harn did not converge, sign, and then refresh the receipt in order" >&2
  cat "$tmp_root/build-harn-dry-run.out" >&2
  exit 1
fi
for producer in "$repo_root/scripts/dev_setup.sh" "$repo_root/scripts/release_gate.sh"; do
  if ! grep -Fq "HARN_BIN='' HARN_BIN_NO_BUILD=0" "$producer"; then
    echo "canonical receipt producer does not clear inherited HARN_BIN: $producer" >&2
    exit 1
  fi
done

# The producer-owned checker snapshot is itself part of the proof. Exercise
# that terminal falsifier last: restoring bytes cannot restore filesystem
# identity, and no later assertion should pretend the receipt is valid again.
printf '\nunproven-proof-replacement\n' >> "$cargo_fixture_proof_checker"
if (
  cd "$cargo_fixture"
  CARGO_TARGET_DIR="$cargo_target" PATH="$no_cargo_bin:$PATH" \
    "$repo_root/scripts/harn_bin.sh" --no-build --print \
    > "$tmp_root/proof-checker-stale.out" \
    2> "$tmp_root/proof-checker-stale.err"
); then
  echo "no-build accepted a changed published freshness checker" >&2
  exit 1
fi
if ! grep -Fq 'freshness checker or manifest changed' \
  "$tmp_root/proof-checker-stale.err"; then
  echo "changed proof-checker failure was not attributable" >&2
  cat "$tmp_root/proof-checker-stale.err" >&2
  exit 1
fi
if [[ -e "$fixture_lease_runner_marker" ]]; then
  echo "Cargo freshness fixture invoked an out-of-scope ambient lease runner" >&2
  exit 1
fi
unset FIXTURE_LEASE_RUNNER_MARKER HARN_CARGO_LEASE_RUNNER
fi

# Return to hermetic fake-Cargo policy probes after the production-shaped
# fixture releases its real shared lease.
export HARN_CARGO_LEASE_MODE=off
missing_target="$tmp_root/missing-target"
: > "$record"
if CARGO_TARGET_DIR="$missing_target" \
  FAKE_CARGO_RECORD="$record" \
  HARN_BIN_NO_BUILD=1 \
  PATH="$fake_cargo_bin:$PATH" \
  "$repo_root/scripts/harn_bin.sh" --print \
  > "$tmp_root/env-no-build-missing.out" \
  2> "$tmp_root/env-no-build-missing.err"; then
  echo "HARN_BIN_NO_BUILD accepted a missing worktree binary" >&2
  exit 1
fi
if ! grep -Fq "no fresh worktree harn binary found" "$tmp_root/env-no-build-missing.err"; then
  echo "HARN_BIN_NO_BUILD missing-binary error was not attributable" >&2
  cat "$tmp_root/env-no-build-missing.err" >&2
  exit 1
fi
if [[ -s "$record" ]]; then
  echo "HARN_BIN_NO_BUILD invoked cargo while reporting a missing binary" >&2
  cat "$record" >&2
  exit 1
fi

if HARN_BIN_NO_BUILD=typo "$repo_root/scripts/harn_bin.sh" --print \
  > "$tmp_root/env-no-build-invalid.out" \
  2> "$tmp_root/env-no-build-invalid.err"; then
  echo "harn_bin accepted an invalid HARN_BIN_NO_BUILD value" >&2
  exit 1
fi
if ! grep -Fq "HARN_BIN_NO_BUILD must be 0 or 1" "$tmp_root/env-no-build-invalid.err"; then
  echo "invalid HARN_BIN_NO_BUILD error was not attributable" >&2
  cat "$tmp_root/env-no-build-invalid.err" >&2
  exit 1
fi

: > "$record"
if CARGO_TARGET_DIR="$target_dir" \
  FAKE_CARGO_RECORD="$record" \
  FAKE_CARGO_MODE=ordinary-failure \
  HARN_BIN_CARGO_TIMEOUT_SECONDS=1 \
  PATH="$fake_cargo_bin:$PATH" \
  "$repo_root/scripts/harn_bin.sh" --print \
  > "$tmp_root/ordinary-failure.out" \
  2> "$tmp_root/ordinary-failure.err"; then
  echo "harn_bin resolver accepted an ordinary Cargo failure" >&2
  exit 1
else
  status=$?
fi
if [[ "$status" -ne 17 ]]; then
  echo "ordinary Cargo failure status changed: expected 17, got $status" >&2
  cat "$tmp_root/ordinary-failure.err" >&2
  exit 1
fi
if [[ "$(grep -Fc "$probe_args" "$record")" -ne 1 ]]; then
  echo "ordinary Cargo failure was retried" >&2
  cat "$record" >&2
  exit 1
fi

: > "$record"
timeout_fifo="$tmp_root/cargo-timeout-ready"
mkfifo "$timeout_fifo"
export FAKE_CARGO_TIMEOUT_FIFO="$timeout_fifo"
child_pid_file="$tmp_root/default-timeout-child.pid"
if RUSTC_WRAPPER=sccache \
  CARGO_TARGET_DIR="$target_dir" \
  FAKE_CARGO_RECORD="$record" \
  FAKE_CARGO_MODE=wrapper-timeout \
  FAKE_CARGO_CHILD_PID_FILE="$child_pid_file" \
  HARN_BIN_CARGO_TIMEOUT_SECONDS=0.1 \
  PATH="$fake_cargo_bin:$PATH" \
  "$repo_root/scripts/harn_bin.sh" --print \
  > "$tmp_root/default-timeout.out" \
  2> "$tmp_root/default-timeout.err"; then
  echo "harn_bin resolver retried a compiler-wrapper timeout by default" >&2
  exit 1
else
  exit_code=$?
fi
if [[ "$exit_code" -ne 124 ]]; then
  echo "compiler-wrapper timeout status changed: expected 124, got $exit_code" >&2
  cat "$tmp_root/default-timeout.err" >&2
  exit 1
fi
if [[ "$(grep -Fc "$probe_args" "$record")" -ne 1 ]]; then
  echo "compiler-wrapper timeout amplified contention with another Cargo probe" >&2
  cat "$record" >&2
  exit 1
fi
if ! grep -Fq "HARN_BIN_RETRY_WITHOUT_WRAPPER=1" "$tmp_root/default-timeout.err"; then
  echo "compiler-wrapper timeout did not offer the explicit recovery control" >&2
  cat "$tmp_root/default-timeout.err" >&2
  exit 1
fi
timed_out_child_pid=$(cat "$child_pid_file")
if kill -0 "$timed_out_child_pid" 2>/dev/null; then
  echo "compiler-wrapper timeout left a descendant process alive: $timed_out_child_pid" >&2
  exit 1
fi

: > "$record"
child_pid_file="$tmp_root/explicit-retry-child.pid"
RUSTC_WRAPPER=sccache \
  CARGO_TARGET_DIR="$target_dir" \
  FAKE_CARGO_RECORD="$record" \
  FAKE_CARGO_MODE=wrapper-timeout \
  FAKE_CARGO_CHILD_PID_FILE="$child_pid_file" \
  HARN_BIN_CARGO_TIMEOUT_SECONDS=0.1 \
  HARN_BIN_RETRY_WITHOUT_WRAPPER=1 \
  PATH="$fake_cargo_bin:$PATH" \
  "$repo_root/scripts/harn_bin.sh" --print \
  > "$tmp_root/timeout-recovery.out" \
  2> "$tmp_root/timeout-recovery.err"
if ! grep -Fxq "$expected_bin" "$tmp_root/timeout-recovery.out"; then
  echo "harn_bin resolver did not recover from a compiler-wrapper timeout" >&2
  cat "$tmp_root/timeout-recovery.err" >&2
  exit 1
fi
if ! grep -Fq "retrying Cargo harn binary probe with the compiler wrapper disabled" "$tmp_root/timeout-recovery.err"; then
  echo "compiler-wrapper timeout recovery was not attributable" >&2
  cat "$tmp_root/timeout-recovery.err" >&2
  exit 1
fi
if [[ "$(grep -Fc "$probe_args" "$record")" -ne 2 ]]; then
  echo "compiler-wrapper timeout did not run exactly one retry" >&2
  cat "$record" >&2
  exit 1
fi
if ! grep -Fxq "RUSTC_WRAPPER=" "$record"; then
  echo "compiler-wrapper timeout retry did not clear RUSTC_WRAPPER" >&2
  cat "$record" >&2
  exit 1
fi
for cleared in \
  "RUSTC_WORKSPACE_WRAPPER=" \
  "CARGO_BUILD_RUSTC_WRAPPER=" \
  "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER=" \
  "SCCACHE_DISABLE=1"; do
  if ! grep -Fxq "$cleared" "$record"; then
    echo "compiler-wrapper timeout retry did not set $cleared" >&2
    cat "$record" >&2
    exit 1
  fi
done
timed_out_child_pid=$(cat "$child_pid_file")
if kill -0 "$timed_out_child_pid" 2>/dev/null; then
  echo "compiler-wrapper timeout left a descendant process alive: $timed_out_child_pid" >&2
  exit 1
fi

: > "$record"
child_pid_file="$tmp_root/retry-timeout-children.pid"
if RUSTC_WORKSPACE_WRAPPER=sccache \
  CARGO_TARGET_DIR="$target_dir" \
  FAKE_CARGO_RECORD="$record" \
  FAKE_CARGO_MODE=wrapper-timeout-always \
  FAKE_CARGO_CHILD_PID_FILE="$child_pid_file" \
  HARN_BIN_CARGO_TIMEOUT_SECONDS=0.1 \
  HARN_BIN_RETRY_WITHOUT_WRAPPER=1 \
  PATH="$fake_cargo_bin:$PATH" \
  "$repo_root/scripts/harn_bin.sh" --print \
  > "$tmp_root/retry-timeout.out" \
  2> "$tmp_root/retry-timeout.err"; then
  echo "harn_bin resolver accepted a timed-out wrapper-disabled retry" >&2
  exit 1
else
  exit_code=$?
fi
if [[ "$exit_code" -ne 124 ]]; then
  echo "wrapper-disabled retry timeout status changed: expected 124, got $exit_code" >&2
  exit 1
fi
if [[ "$(grep -Fc 'hint: to reuse a binary you already built:' "$tmp_root/retry-timeout.err")" -ne 1 ]]; then
  echo "wrapper-disabled retry timeout did not print one terminal hint block" >&2
  cat "$tmp_root/retry-timeout.err" >&2
  exit 1
fi
if [[ "$(grep -Fc "$probe_args" "$record")" -ne 2 ]]; then
  echo "wrapper-disabled retry timeout did not run exactly two probes" >&2
  cat "$record" >&2
  exit 1
fi
while IFS= read -r timed_out_child_pid; do
  if kill -0 "$timed_out_child_pid" 2>/dev/null; then
    echo "wrapper-disabled retry timeout left a descendant alive: $timed_out_child_pid" >&2
    exit 1
  fi
done < "$child_pid_file"

: > "$record"
child_pid_file="$tmp_root/retry-failure-child.pid"
if RUSTC_WRAPPER=sccache \
  CARGO_TARGET_DIR="$target_dir" \
  FAKE_CARGO_RECORD="$record" \
  FAKE_CARGO_MODE=wrapper-timeout-retry-failure \
  FAKE_CARGO_CHILD_PID_FILE="$child_pid_file" \
  HARN_BIN_CARGO_TIMEOUT_SECONDS=0.1 \
  HARN_BIN_RETRY_WITHOUT_WRAPPER=1 \
  PATH="$fake_cargo_bin:$PATH" \
  "$repo_root/scripts/harn_bin.sh" --print \
  > "$tmp_root/retry-failure.out" \
  2> "$tmp_root/retry-failure.err"; then
  echo "harn_bin resolver accepted a failed wrapper-disabled retry" >&2
  exit 1
else
  status=$?
fi
if [[ "$status" -ne 19 ]]; then
  echo "wrapper-disabled retry failure status changed: expected 19, got $status" >&2
  cat "$tmp_root/retry-failure.err" >&2
  exit 1
fi

# A probe that runs out of time with no compiler wrapper to disable has no
# retry left, and a cold workspace build legitimately exceeds the deadline. The
# bare timeout reads like a hang, so the failure has to say how to get past it.
: > "$record"
plain_timeout_child="$tmp_root/plain-timeout-child.pid"
if CARGO_TARGET_DIR="$target_dir" \
  FAKE_CARGO_RECORD="$record" \
  FAKE_CARGO_MODE=plain-timeout \
  FAKE_CARGO_CHILD_PID_FILE="$plain_timeout_child" \
  HARN_BIN_CARGO_TIMEOUT_SECONDS=0.1 \
  PATH="$fake_cargo_bin:$PATH" \
  "$repo_root/scripts/harn_bin.sh" --print \
  > "$tmp_root/plain-timeout.out" \
  2> "$tmp_root/plain-timeout.err"; then
  echo "harn_bin resolver accepted an unrecoverable probe timeout" >&2
  exit 1
else
  status=$?
fi
if [[ "$status" -ne 124 ]]; then
  echo "unrecoverable probe timeout status changed: expected 124, got $status" >&2
  cat "$tmp_root/plain-timeout.err" >&2
  exit 1
fi
if ! grep -Fq "HARN_BIN=<path-to-harn> HARN_BIN_NO_BUILD=1" "$tmp_root/plain-timeout.err"; then
  echo "probe timeout did not offer the prebuilt-binary escape hatch" >&2
  cat "$tmp_root/plain-timeout.err" >&2
  exit 1
fi
if ! grep -Fq "HARN_BIN_CARGO_TIMEOUT_SECONDS=" "$tmp_root/plain-timeout.err"; then
  echo "probe timeout did not offer a longer deadline" >&2
  cat "$tmp_root/plain-timeout.err" >&2
  exit 1
fi

if HARN_BIN_CARGO_TIMEOUT_SECONDS=invalid \
  CARGO_TARGET_DIR="$target_dir" \
  FAKE_CARGO_RECORD="$record" \
  PATH="$fake_cargo_bin:$PATH" \
  "$repo_root/scripts/harn_bin.sh" --print \
  > "$tmp_root/invalid-timeout.out" \
  2> "$tmp_root/invalid-timeout.err"; then
  echo "harn_bin resolver accepted an invalid Cargo probe timeout" >&2
  exit 1
else
  status=$?
fi
if [[ "$status" -ne 2 ]] || ! grep -Fq "must be a positive number" "$tmp_root/invalid-timeout.err"; then
  echo "invalid Cargo probe timeout was not reported with status 2" >&2
  cat "$tmp_root/invalid-timeout.err" >&2
  exit 1
fi

: > "$record"
if HARN_BIN_RETRY_WITHOUT_WRAPPER=invalid \
  CARGO_TARGET_DIR="$target_dir" \
  FAKE_CARGO_RECORD="$record" \
  PATH="$fake_cargo_bin:$PATH" \
  "$repo_root/scripts/harn_bin.sh" --print \
  > "$tmp_root/invalid-retry.out" \
  2> "$tmp_root/invalid-retry.err"; then
  echo "harn_bin resolver accepted an invalid wrapper retry policy" >&2
  exit 1
else
  exit_code=$?
fi
if [[ "$exit_code" -ne 2 ]] || ! grep -Fq "must be 0 or 1" "$tmp_root/invalid-retry.err"; then
  echo "invalid wrapper retry policy was not reported with status 2" >&2
  cat "$tmp_root/invalid-retry.err" >&2
  exit 1
fi
if [[ -s "$record" ]]; then
  echo "invalid wrapper retry policy invoked Cargo before validation" >&2
  cat "$record" >&2
  exit 1
fi

registry="$repo_root/crates/harn-vm/src/environment_registry_names.txt"
resolver_names="$(grep -Eho 'HARN_[A-Z0-9_]+' \
  "$repo_root/scripts/harn_bin.sh" "$repo_root/scripts/lib/harn_bin.sh" | sort -u)"
if [[ "$(wc -l <<<"$resolver_names" | tr -d ' ')" -lt 4 ]]; then
  echo "registry guard discovered too few harn_bin environment controls" >&2
  exit 1
fi
while IFS= read -r name; do
  if ! grep -Fxq "$name" "$registry"; then
    echo "harn_bin environment control is absent from the registry: $name" >&2
    exit 1
  fi
done <<<"$resolver_names"

if ! grep -Fq \
  'HARN_BIN_RESOLVER_TEST_ALLOW_CARGO=1 ./scripts/tests/harn_bin_resolver_test.sh' \
  "$repo_root/Makefile"; then
  echo "production-shaped resolver falsifiers are not projected into the post-warm CI lane" >&2
  exit 1
fi

echo "harn_bin_resolver_test: ok"
