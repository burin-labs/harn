#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

fake_cargo="$tmp_root/fake cargo"
args_log="$tmp_root/args.log"
cat > "$fake_cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if env | grep -q '^HARN_TEST_ONE_'; then
  echo 'test-one control variables leaked into the Cargo runner' >&2
  exit 19
fi
: "${TEST_ONE_ARGS_LOG:?}"
printf '%s\0' "$@" > "$TEST_ONE_ARGS_LOG"
case "${TEST_ONE_FAKE_MODE:-success}" in
  success)
    printf '%s\n' \
      'running 1 test' \
      'test package::module::case ... ok' \
      'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 99 filtered out'
    ;;
  zero)
    printf '%s\n' \
      'running 0 tests' \
      'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 100 filtered out'
    ;;
  failure)
    echo 'cargo failed' >&2
    exit 17
    ;;
  *)
    echo "unexpected fake mode: $TEST_ONE_FAKE_MODE" >&2
    exit 2
    ;;
esac
SH
chmod +x "$fake_cargo"

injection_target="$tmp_root/should-not-run"
exact_name="package::module::case; \$(touch $injection_target)"
TEST_ONE_ARGS_LOG="$args_log" \
  HARN_TEST_ONE_CARGO_RUNNER="$fake_cargo" \
  "$repo_root/scripts/test_one.sh" --package harn-cli --lib "$exact_name" \
  > "$tmp_root/success.out"
printf '%s\0' \
  test --package harn-cli --lib "$exact_name" -- --exact --format terse \
  > "$tmp_root/expected-args.log"
if ! cmp -s "$tmp_root/expected-args.log" "$args_log"; then
  echo "exact-test arguments were not preserved byte-for-byte" >&2
  exit 1
fi
if [[ -e "$injection_target" ]]; then
  echo "exact test name was evaluated as shell code" >&2
  exit 1
fi

TEST_ONE_ARGS_LOG="$args_log" \
  HARN_TEST_ONE_CARGO_RUNNER="$fake_cargo" \
  "$repo_root/scripts/test_one.sh" --package harn-cli \
  --test harn_cli_fast package::module::case > "$tmp_root/binary.out"
printf '%s\0' \
  test --package harn-cli --test harn_cli_fast package::module::case \
  -- --exact --format terse > "$tmp_root/expected-binary-args.log"
if ! cmp -s "$tmp_root/expected-binary-args.log" "$args_log"; then
  echo "named test-binary selector was not preserved" >&2
  exit 1
fi

if TEST_ONE_ARGS_LOG="$args_log" \
  HARN_TEST_ONE_CARGO_RUNNER="$fake_cargo" \
  TEST_ONE_FAKE_MODE=zero \
  "$repo_root/scripts/test_one.sh" --package harn-cli --lib missing::test \
  > "$tmp_root/zero.out" 2> "$tmp_root/zero.err"; then
  echo "zero-match exact test unexpectedly succeeded" >&2
  exit 1
fi
if ! grep -Fq "did not produce a one-test success receipt" "$tmp_root/zero.err"; then
  echo "zero-match failure was not attributable" >&2
  cat "$tmp_root/zero.err" >&2
  exit 1
fi

set +e
TEST_ONE_ARGS_LOG="$args_log" \
  HARN_TEST_ONE_CARGO_RUNNER="$fake_cargo" \
  TEST_ONE_FAKE_MODE=failure \
  "$repo_root/scripts/test_one.sh" --package harn-cli --lib package::module::case \
  > "$tmp_root/failure.out" 2> "$tmp_root/failure.err"
failure_status=$?
set -e
if [[ $failure_status -ne 17 ]]; then
  echo "Cargo failure status was not preserved: $failure_status" >&2
  exit 1
fi

if "$repo_root/scripts/test_one.sh" --package harn-cli --lib \
  > "$tmp_root/missing.out" 2> "$tmp_root/missing.err"; then
  echo "missing exact test name unexpectedly succeeded" >&2
  exit 1
fi
if "$repo_root/scripts/test_one.sh" --package harn-cli --lib \
  --test harn_cli_fast package::module::case \
  > "$tmp_root/duplicate.out" 2> "$tmp_root/duplicate.err"; then
  echo "multiple test-target selectors unexpectedly succeeded" >&2
  exit 1
fi

HARN_TEST_ONE_NAME=package::module::case \
  TEST_ONE_ARGS_LOG="$args_log" \
  HARN_TEST_ONE_CARGO_RUNNER="$fake_cargo" \
  make -s -C "$repo_root" test-one > "$tmp_root/make.out"

HARN_TEST_ONE_NAME=package::module::case \
  HARN_TEST_ONE_PACKAGE=harn-hostlib \
  HARN_TEST_ONE_BINARY=harn_hostlib \
  TEST_ONE_ARGS_LOG="$args_log" \
  HARN_TEST_ONE_CARGO_RUNNER="$fake_cargo" \
  make -s -C "$repo_root" test-one > "$tmp_root/make-binary.out"
printf '%s\0' \
  test --package harn-hostlib --test harn_hostlib package::module::case \
  -- --exact --format terse > "$tmp_root/expected-make-binary-args.log"
if ! cmp -s "$tmp_root/expected-make-binary-args.log" "$args_log"; then
  echo "Make test-one did not preserve the named integration-test selector" >&2
  exit 1
fi

echo "test_one contract tests passed"
