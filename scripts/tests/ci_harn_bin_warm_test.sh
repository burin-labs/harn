#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)

tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

fake_bin="$tmp_root/bin"
target_dir="$tmp_root/target dir"
record="$tmp_root/cargo-record.txt"
github_env="$tmp_root/github-env.txt"
mkdir -p "$fake_bin" "$target_dir/debug"

cat > "$fake_bin/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
{
  printf 'args=%s\n' "$*"
  printf 'CARGO_TARGET_DIR=%s\n' "${CARGO_TARGET_DIR-__unset__}"
  printf 'CARGO_BUILD_BUILD_DIR=%s\n' "${CARGO_BUILD_BUILD_DIR-__unset__}"
} >> "$FAKE_CARGO_RECORD"
case "$*" in
  "build --quiet --bin harn --bin harn-freshness-check --features internal-freshness-checker")
    mkdir -p "${CARGO_TARGET_DIR:?}/debug"
    cat > "$CARGO_TARGET_DIR/debug/harn" <<'BIN'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" = "__internal-freshness-evidence-v4" ]]; then
  if [[ -n "${7:-}" ]]; then printf "harn-freshness-manifest-v2\n" >"$7"; fi
  binary_hash="$(git hash-object --no-filters -- "$3")000000000000000000000000"
  dep_hash="$(git hash-object --no-filters -- "$2")000000000000000000000000"
  printf 'harn-artifact-evidence-v4-depfile-0.1.1-manifest-2\nbuild-freshness=%s\nbuild-id=%s\nartifact-stat=%s\ndep-info=%s\ndependencies=%s\n' \
    "$(cat "$3.build-freshness")" "$binary_hash" "$binary_hash" \
    "$dep_hash" "$dep_hash"
  exit 0
fi
if [[ "${1:-}" = "__internal-executable-path" ]]; then
  printf '%s\n' "$0"
  exit 0
fi
printf 'fake harn\n'
BIN
    chmod +x "$CARGO_TARGET_DIR/debug/harn"
    cp "$FAKE_FRESHNESS_CHECKER" "$CARGO_TARGET_DIR/debug/harn-freshness-check"
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
chmod +x "$fake_bin/cargo"
export FAKE_FRESHNESS_CHECKER="$repo_root/scripts/tests/fixtures/harn_bin/fake_freshness_checker.sh"

unset HARN_BIN HARN_BIN_NO_BUILD
export HARN_CARGO_LEASE_MODE=off

env -u CARGO_TARGET_DIR -u CARGO_BUILD_BUILD_DIR \
  CARGO_TARGET_DIR="$target_dir" \
  FAKE_CARGO_RECORD="$record" \
  GITHUB_ENV="$github_env" \
  PATH="$fake_bin:$PATH" \
  "$repo_root/scripts/ci_warm_harn_bin.sh" > "$tmp_root/warm.out"

expected_bin="$target_dir/debug/harn"
if ! grep -Fxq "args=build --quiet --bin harn --bin harn-freshness-check --features internal-freshness-checker" "$record"; then
  echo "ci_warm_harn_bin did not resolve harn through Cargo" >&2
  cat "$record" >&2
  exit 1
fi
if ! grep -Fxq "CARGO_TARGET_DIR=$target_dir" "$record"; then
  echo "ci_warm_harn_bin did not pass through CARGO_TARGET_DIR" >&2
  cat "$record" >&2
  exit 1
fi
if ! grep -Fxq "CARGO_BUILD_BUILD_DIR=$target_dir" "$record"; then
  echo "ci_warm_harn_bin did not reuse CARGO_TARGET_DIR for Cargo intermediates" >&2
  cat "$record" >&2
  exit 1
fi
if ! grep -Fxq "ok: harn-bin ($expected_bin)" "$tmp_root/warm.out"; then
  echo "ci_warm_harn_bin did not report the expected binary" >&2
  cat "$tmp_root/warm.out" >&2
  exit 1
fi
if ! grep -Fxq "HARN_BIN=$expected_bin" "$github_env"; then
  echo "ci_warm_harn_bin did not write HARN_BIN to GITHUB_ENV" >&2
  cat "$github_env" >&2
  exit 1
fi

: > "$record"
: > "$github_env"
custom_build_dir="$tmp_root/custom build dir"
rm -f "$expected_bin"
CARGO_TARGET_DIR="$target_dir" \
  CARGO_BUILD_BUILD_DIR="$custom_build_dir" \
  FAKE_CARGO_RECORD="$record" \
  GITHUB_ENV="$github_env" \
  PATH="$fake_bin:$PATH" \
  "$repo_root/scripts/ci_warm_harn_bin.sh" > "$tmp_root/warm-custom-build-dir.out"

if ! grep -Fxq "CARGO_BUILD_BUILD_DIR=$custom_build_dir" "$record"; then
  echo "ci_warm_harn_bin did not preserve explicit CARGO_BUILD_BUILD_DIR" >&2
  cat "$record" >&2
  exit 1
fi

: > "$record"
: > "$github_env"
HARN_BIN="$expected_bin" \
  CARGO_TARGET_DIR="$target_dir" \
  FAKE_CARGO_RECORD="$record" \
  GITHUB_ENV="$github_env" \
  PATH="$fake_bin:$PATH" \
  "$repo_root/scripts/ci_warm_harn_bin.sh" > "$tmp_root/reuse.out"

if [[ -s "$record" ]]; then
  echo "ci_warm_harn_bin rebuilt despite an executable HARN_BIN" >&2
  cat "$record" >&2
  exit 1
fi
if ! grep -Fxq "ok: harn-bin ($expected_bin)" "$tmp_root/reuse.out"; then
  echo "ci_warm_harn_bin did not report the reused binary" >&2
  cat "$tmp_root/reuse.out" >&2
  exit 1
fi
if ! grep -Fxq "HARN_BIN=$expected_bin" "$github_env"; then
  echo "ci_warm_harn_bin did not persist the reused HARN_BIN" >&2
  cat "$github_env" >&2
  exit 1
fi

echo "ci_harn_bin_warm_test: ok"
