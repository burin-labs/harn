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
if env | grep -q '^HARN_NEXTEST_'; then
  echo 'focused-nextest control variables leaked into the Cargo runner' >&2
  exit 19
fi
: "${NEXTEST_ARGS_LOG:?}"
printf '%s\0' "$@" > "$NEXTEST_ARGS_LOG"
SH
chmod +x "$fake_cargo"

injection_target="$tmp_root/should-not-run"
filter="test(check_cli::reports_clean_program) or test(check_cli::reports_parse_error); \$(touch $injection_target)"
HARN_NEXTEST_FILTER="$filter" \
  HARN_NEXTEST_PACKAGE=harn-cli \
  HARN_NEXTEST_BINARY=harn_cli_fast \
  HARN_NEXTEST_CARGO_RUNNER="$fake_cargo" \
  NEXTEST_ARGS_LOG="$args_log" \
  make -s -C "$repo_root" test-focused
printf '%s\0' \
  nextest run --package harn-cli --test harn_cli_fast -E "$filter" \
  > "$tmp_root/expected-args.log"
if ! cmp -s "$tmp_root/expected-args.log" "$args_log"; then
  echo "focused nextest arguments were not preserved byte-for-byte" >&2
  exit 1
fi
if [[ -e "$injection_target" ]]; then
  echo "focused nextest expression was evaluated as shell code" >&2
  exit 1
fi

HARN_NEXTEST_FILTER='test(unit_case)' \
  HARN_NEXTEST_CARGO_RUNNER="$fake_cargo" \
  NEXTEST_ARGS_LOG="$args_log" \
  "$repo_root/scripts/test_focused.sh"
printf '%s\0' nextest run --workspace -E 'test(unit_case)' \
  > "$tmp_root/expected-workspace-args.log"
if ! cmp -s "$tmp_root/expected-workspace-args.log" "$args_log"; then
  echo "focused nextest workspace selector was not preserved" >&2
  exit 1
fi

if HARN_NEXTEST_CARGO_RUNNER="$fake_cargo" \
  NEXTEST_ARGS_LOG="$args_log" \
  "$repo_root/scripts/test_focused.sh" \
  > "$tmp_root/missing.out" 2> "$tmp_root/missing.err"; then
  echo "missing nextest filter unexpectedly succeeded" >&2
  exit 1
fi
if ! grep -Fq 'HARN_NEXTEST_FILTER must contain' "$tmp_root/missing.err"; then
  echo "missing nextest filter failure was not attributable" >&2
  exit 1
fi

if HARN_NEXTEST_FILTER='test(case)' \
  HARN_NEXTEST_BINARY=harn_cli_fast \
  HARN_NEXTEST_CARGO_RUNNER="$fake_cargo" \
  NEXTEST_ARGS_LOG="$args_log" \
  "$repo_root/scripts/test_focused.sh" \
  > "$tmp_root/binary.out" 2> "$tmp_root/binary.err"; then
  echo "unscoped integration binary unexpectedly succeeded" >&2
  exit 1
fi
if ! grep -Fq 'HARN_NEXTEST_BINARY requires HARN_NEXTEST_PACKAGE' \
    "$tmp_root/binary.err"; then
  echo "unscoped integration binary failure was not attributable" >&2
  exit 1
fi

echo "test_focused contract tests passed"
