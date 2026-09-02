#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
tmp_root=$(mktemp -d)
holder_pid=""
cleanup() {
  if [[ -n "$holder_pid" ]]; then
    kill "$holder_pid" 2>/dev/null || true
    wait "$holder_pid" 2>/dev/null || true
  fi
  rm -rf "$tmp_root"
}
trap cleanup EXIT

fake_cargo="$tmp_root/fake cargo"
args_log="$tmp_root/args.log"
metadata_json="$tmp_root/metadata.json"
cat > "$metadata_json" <<'JSON'
{
  "packages": [
    {
      "name": "harn-cli",
      "targets": [
        { "kind": ["lib"], "name": "harn_cli" },
        { "kind": ["bin"], "name": "harn" },
        { "kind": ["test"], "name": "harn_cli_fast" }
      ]
    }
  ]
}
JSON
cat > "$fake_cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if env | grep -q '^HARN_TEST_ONE_'; then
  echo 'test-one control variables leaked into the Cargo runner' >&2
  exit 19
fi
: "${TEST_ONE_ARGS_LOG:?}"
if [[ "${1:-}" == "metadata" ]]; then
  cat "${TEST_ONE_FAKE_METADATA:?}"
  exit 0
fi
printf '%s\0' "$@" > "$TEST_ONE_ARGS_LOG"
for argument in "$@"; do
  if [[ "$argument" == "--list" ]]; then
    # An empty listing is how a real target reports that it does not define the
    # requested name, so the fixture passes its listing through verbatim.
    if [[ -n "${TEST_ONE_FAKE_LISTING:-}" ]]; then
      printf '%s\n' "$TEST_ONE_FAKE_LISTING"
    fi
    exit 0
  fi
done
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
  held-fd)
    # Model a runner-side supervisor that survives the Cargo command while
    # retaining its stdout descriptor. The wrapper must return with this child
    # still alive instead of waiting for pipe EOF.
    sleep 30 &
    holder_pid=$!
    disown "$holder_pid"
    printf '%s\n' "$holder_pid" > "${TEST_ONE_FAKE_HOLDER_PID:?}"
    printf '%s\n' \
      'running 1 test' \
      'test package::module::case ... ok' \
      'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 99 filtered out'
    ;;
  *)
    echo "unexpected fake mode: $TEST_ONE_FAKE_MODE" >&2
    exit 2
    ;;
esac
SH
chmod +x "$fake_cargo"

export TEST_ONE_ARGS_LOG="$args_log"
export TEST_ONE_FAKE_METADATA="$metadata_json"
export HARN_TEST_ONE_CARGO_RUNNER="$fake_cargo"

injection_target="$tmp_root/should-not-run"
exact_name="package::module::case; \$(touch $injection_target)"
TEST_ONE_FAKE_LISTING="$exact_name: test" \
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

TEST_ONE_FAKE_LISTING='package::module::case: test' \
  "$repo_root/scripts/test_one.sh" --package harn-cli \
  --test harn_cli_fast package::module::case > "$tmp_root/binary.out"
printf '%s\0' \
  test --package harn-cli --test harn_cli_fast package::module::case \
  -- --exact --format terse > "$tmp_root/expected-binary-args.log"
if ! cmp -s "$tmp_root/expected-binary-args.log" "$args_log"; then
  echo "named test-binary selector was not preserved" >&2
  exit 1
fi

# A name the requested target does not define is refused before the run, not
# handed to a filter that cannot match it. The refusal names the request and
# the package's real targets so the caller can pick a servable one.
if TEST_ONE_FAKE_LISTING='' \
  "$repo_root/scripts/test_one.sh" --package harn-cli --lib \
  parser_corpus::case_defined_in_an_integration_binary \
  > "$tmp_root/wrong-kind.out" 2> "$tmp_root/wrong-kind.err"; then
  echo "unservable target kind unexpectedly succeeded" >&2
  exit 1
fi
if ! grep -Fq "defines no test named" "$tmp_root/wrong-kind.err"; then
  echo "unservable target kind was not attributable" >&2
  cat "$tmp_root/wrong-kind.err" >&2
  exit 1
fi
if ! grep -Fq "test target: harn_cli_fast" "$tmp_root/wrong-kind.err"; then
  echo "refusal did not name the package's available targets" >&2
  cat "$tmp_root/wrong-kind.err" >&2
  exit 1
fi
if ! grep -Fq -- "--test <binary>" "$tmp_root/wrong-kind.err"; then
  echo "refusal did not name the selector that could serve the request" >&2
  cat "$tmp_root/wrong-kind.err" >&2
  exit 1
fi
# The refusal must be a refusal: the last thing handed to Cargo is the listing
# probe, never the run. Without this the case would also pass if the run went
# ahead and the receipt check caught it afterwards.
if ! tr '\0' '\n' < "$args_log" | grep -Fqx -- "--list"; then
  echo "unservable request reached a run instead of stopping at the probe" >&2
  tr '\0' '\n' < "$args_log" >&2
  exit 1
fi

if "$repo_root/scripts/test_one.sh" --package harn-cli --test not_a_target \
  package::module::case > "$tmp_root/no-target.out" 2> "$tmp_root/no-target.err"; then
  echo "unknown test binary unexpectedly succeeded" >&2
  exit 1
fi
if ! grep -Fq "has no integration-test binary 'not_a_target'" "$tmp_root/no-target.err"; then
  echo "unknown test binary was not attributable" >&2
  cat "$tmp_root/no-target.err" >&2
  exit 1
fi

if "$repo_root/scripts/test_one.sh" --package not-a-package --lib \
  package::module::case > "$tmp_root/no-package.out" 2> "$tmp_root/no-package.err"; then
  echo "unknown package unexpectedly succeeded" >&2
  exit 1
fi
if ! grep -Fq "no package named 'not-a-package'" "$tmp_root/no-package.err"; then
  echo "unknown package was not attributable" >&2
  cat "$tmp_root/no-package.err" >&2
  exit 1
fi

# A target that lists the name but then runs nothing is still a failure: the
# receipt check stays behind the reachability probe rather than being replaced
# by it.
if TEST_ONE_FAKE_LISTING='missing::test: test' \
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
TEST_ONE_FAKE_LISTING='package::module::case: test' \
  TEST_ONE_FAKE_MODE=failure \
  "$repo_root/scripts/test_one.sh" --package harn-cli --lib package::module::case \
  > "$tmp_root/failure.out" 2> "$tmp_root/failure.err"
failure_status=$?
set -e
if [[ $failure_status -ne 17 ]]; then
  echo "Cargo failure status was not preserved: $failure_status" >&2
  exit 1
fi

# A descendant holding the runner's stdout open must not own the exact-test
# command's lifetime. Start the wrapper asynchronously so the control can fail
# in bounded time without relying on a platform-specific timeout utility.
holder_pid_file="$tmp_root/holder.pid"
held_fd_output="$tmp_root/held-fd.out"
TEST_ONE_FAKE_LISTING='package::module::case: test' \
  TEST_ONE_FAKE_MODE=held-fd \
  TEST_ONE_FAKE_HOLDER_PID="$holder_pid_file" \
  "$repo_root/scripts/test_one.sh" --package harn-cli --lib package::module::case \
  > "$held_fd_output" 2> "$tmp_root/held-fd.err" &
held_fd_wrapper_pid=$!
held_fd_finished=false
for _ in {1..100}; do
  if ! kill -0 "$held_fd_wrapper_pid" 2>/dev/null; then
    held_fd_finished=true
    break
  fi
  sleep 0.05
done
if [[ "$held_fd_finished" != true ]]; then
  kill "$held_fd_wrapper_pid" 2>/dev/null || true
  wait "$held_fd_wrapper_pid" 2>/dev/null || true
  if [[ -s "$holder_pid_file" ]]; then
    kill "$(< "$holder_pid_file")" 2>/dev/null || true
  fi
  echo "exact-test wrapper waited for a descendant holding stdout" >&2
  exit 1
fi
wait "$held_fd_wrapper_pid"
if [[ ! -s "$holder_pid_file" ]]; then
  echo "inherited-stdout control did not start its background holder" >&2
  exit 1
fi
holder_pid=$(< "$holder_pid_file")
if ! kill -0 "$holder_pid" 2>/dev/null; then
  echo "inherited-stdout holder was not alive when the wrapper returned" >&2
  exit 1
fi
kill "$holder_pid" 2>/dev/null || true
wait "$holder_pid" 2>/dev/null || true
holder_pid=""
if ! grep -Fq 'test result: ok. 1 passed;' "$held_fd_output"; then
  echo "inherited-stdout control lost the success receipt" >&2
  cat "$held_fd_output" >&2
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
  TEST_ONE_FAKE_LISTING='package::module::case: test' \
  make -s -C "$repo_root" test-one > "$tmp_root/make.out"

# The Make boundary carries the target kind too, so a caller does not have to
# reach past it to run an integration test.
HARN_TEST_ONE_NAME=package::module::case \
  HARN_TEST_ONE_PACKAGE=harn-cli \
  HARN_TEST_ONE_BINARY=harn_cli_fast \
  TEST_ONE_FAKE_LISTING='package::module::case: test' \
  make -s -C "$repo_root" test-one > "$tmp_root/make-binary.out"
if ! cmp -s "$tmp_root/expected-binary-args.log" "$args_log"; then
  echo "HARN_TEST_ONE_BINARY did not reach the integration-test selector" >&2
  tr '\0' '\n' < "$args_log" >&2
  exit 1
fi

echo "test_one contract tests passed"
