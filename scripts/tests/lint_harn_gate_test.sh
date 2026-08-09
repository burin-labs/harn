#!/bin/sh
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/harn-lint-gate.XXXXXX")
trap 'rm -rf "$tmp"' EXIT HUP INT TERM

mkdir -p "$tmp/conformance/tests"
printf 'fn main() {}\n' > "$tmp/conformance/tests/clean.harn"
printf 'fn main() {}\n' > "$tmp/conformance/tests/warning.harn"
printf 'fn main() {}\n' > "$tmp/conformance/tests/info.harn"
printf 'fn main() {}\n' > "$tmp/conformance/tests/broken.harn"
mkdir -p "$tmp/conformance/tests/hidden"
printf 'hidden fixture\n' > "$tmp/conformance/tests/hidden/.harn"
printf 'not harn\n' > "$tmp/conformance/tests/excluded.harn"
printf 'expected parse failure\n' > "$tmp/conformance/tests/excluded.error"
mode_file="$tmp/mode"
calls_file="$tmp/calls"
fake_harn="$tmp/harn"

cat > "$fake_harn" <<'EOF'
#!/bin/sh
set -eu

if [ "${1:-}" = "--print" ]; then
  printf '%s\n' "$0"
  exit 0
fi
if [ "${1:-}" = "--" ]; then
  shift
fi

if [ "${1:-}" = "check" ] && [ "${2:-}" = "--json" ] && [ "${3:-}" = "--independent" ]; then
  shift 3
  printf 'call\n' >> "$FAKE_HARN_CALLS"
  printf '%s\n' "$@" >> "$FAKE_HARN_CALLS"
  if [ "$#" -eq 1 ] && [ "${1##*/}" = ".harn" ]; then
    printf '%s\n' '{"schemaVersion":1,"ok":false,"data":null,"error":{"code":"no_harn_files","message":"sentinel","details":null},"warnings":[]}'
    exit 1
  fi
  case "$(cat "$FAKE_HARN_MODE")" in
    clean)
      jq -n --args '
        {schemaVersion: 1, ok: true,
         data: {files: ($ARGS.positional | map(
           if endswith("/info.harn")
           then {path: ., status: "ok", diagnostics: [{source: "lint", severity: "info", code: "HARN-LNT-998", message: "sentinel"}]}
           else {path: ., status: "ok", diagnostics: []}
           end)),
           summary: {ok: ($ARGS.positional | length), warnings: 0, errors: 0, diagnostics: 1}},
         error: null, warnings: []}
      ' "$@"
      exit 0
      ;;
    checker-failure)
      printf '%s\n' 'checker failed before diagnostics were rendered' >&2
      exit 23
      ;;
    warning)
      jq -n --args '
        {schemaVersion: 1, ok: true,
         data: {files: ($ARGS.positional | map(
           if endswith("/warning.harn")
           then {path: ., status: "warning", diagnostics: [{source: "lint", severity: "warning", code: "HARN-LNT-999", message: "sentinel"}]}
           else {path: ., status: "ok", diagnostics: []}
           end)),
           summary: {ok: 2, warnings: 1, errors: 0, diagnostics: 1}},
         error: null, warnings: []}
      ' "$@"
      exit 0
      ;;
    error)
      jq -n --args '
        {schemaVersion: 1, ok: false,
         data: {files: ($ARGS.positional | map(
           if endswith("/broken.harn")
           then {path: ., status: "error", diagnostics: [{source: "check", severity: "error", code: "HARN-CHK-999", message: "sentinel"}]}
           else {path: ., status: "ok", diagnostics: []}
           end)),
           summary: {ok: 2, warnings: 0, errors: 1, diagnostics: 1}},
         error: {code: "check_failed", message: "sentinel", details: null}, warnings: []}
      ' "$@"
      exit 1
      ;;
  esac
fi
exit 0
EOF
chmod +x "$fake_harn"

baseline="$tmp/baseline.tsv"
printf '1\thidden/.harn\t__CHECK_FAILED__\n' > "$baseline"

run_gate() {
  : > "$calls_file"
  FAKE_HARN_MODE="$mode_file" \
    FAKE_HARN_CALLS="$calls_file" \
    HARN_BIN="$fake_harn" \
    CONFORMANCE_LINT_ROOT="$tmp/conformance/tests" \
    CONFORMANCE_LINT_BASELINE="$baseline" \
    HARN_CHECK_JOBS=1 \
    "$repo_root/scripts/check-conformance-lint-baseline.sh"
}

printf '%s\n' clean > "$mode_file"
run_gate > "$tmp/clean.log" 2>&1
test "$(grep -c '^call$' "$calls_file")" -eq 2
test "$(grep -Fxc "$tmp/conformance/tests/clean.harn" "$calls_file")" -eq 1
test "$(grep -Fxc "$tmp/conformance/tests/info.harn" "$calls_file")" -eq 1
test "$(grep -Fxc "$tmp/conformance/tests/hidden/.harn" "$calls_file")" -eq 1
grep -Fxq "$tmp/conformance/tests/clean.harn" "$calls_file"
grep -Fxq "$tmp/conformance/tests/warning.harn" "$calls_file"
grep -Fxq "$tmp/conformance/tests/broken.harn" "$calls_file"
if grep -Fq "$tmp/conformance/tests/excluded.harn" "$calls_file"; then
  echo "lint-harn included a paired negative fixture" >&2
  exit 1
fi

printf '%s\n' checker-failure > "$mode_file"
if run_gate > "$tmp/checker-failure.log" 2>&1; then
  echo "lint-harn passed after harn check exited nonzero" >&2
  exit 1
fi
grep -q '__CHECK_FAILED__' "$tmp/checker-failure.log"

printf '%s\n' warning > "$mode_file"
if run_gate > "$tmp/warning.log" 2>&1; then
  echo "lint-harn passed a structured warning diagnostic" >&2
  exit 1
fi
grep -q 'HARN-LNT-999' "$tmp/warning.log"

printf '%s\n' error > "$mode_file"
printf '1\tbroken.harn\tHARN-CHK-999\n1\tbroken.harn\t__CHECK_FAILED__\n1\thidden/.harn\t__CHECK_FAILED__\n' > "$baseline"
run_gate > "$tmp/error.log" 2>&1

echo "lint-harn gate regression checks passed"
