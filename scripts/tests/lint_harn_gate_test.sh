#!/bin/sh
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/harn-lint-gate.XXXXXX")
trap 'rm -rf "$tmp"' EXIT HUP INT TERM

mkdir -p "$tmp/conformance/tests"
printf 'fn main() {}\n' > "$tmp/conformance/tests/clean.harn"
mode_file="$tmp/mode"
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

if [ "${1:-}" = "check" ]; then
  case "$(cat "$FAKE_HARN_MODE")" in
    clean) exit 0 ;;
    checker-failure)
      printf '%s\n' 'checker failed before diagnostics were rendered' >&2
      exit 23
      ;;
    warning)
      printf '%s\n' 'warning[HARN-LNT-999]: mutation sentinel'
      exit 0
      ;;
  esac
fi
exit 0
EOF
chmod +x "$fake_harn"

baseline="$tmp/baseline.tsv"
: > "$baseline"

run_gate() {
  FAKE_HARN_MODE="$mode_file" \
    HARN_BIN="$fake_harn" \
    CONFORMANCE_LINT_ROOT="$tmp/conformance/tests" \
    CONFORMANCE_LINT_BASELINE="$baseline" \
    CONFORMANCE_LINT_JOBS=1 \
    "$repo_root/scripts/check-conformance-lint-baseline.sh"
}

printf '%s\n' clean > "$mode_file"
run_gate > "$tmp/clean.log" 2>&1

printf '%s\n' checker-failure > "$mode_file"
if run_gate > "$tmp/checker-failure.log" 2>&1; then
  echo "lint-harn passed after harn check exited nonzero" >&2
  exit 1
fi
grep -q '__CHECK_FAILED__' "$tmp/checker-failure.log"

printf '%s\n' warning > "$mode_file"
printf '%s\n' 'HARN-LNT-999' > "$tmp/conformance/tests/clean.lint"
run_gate > "$tmp/declared-warning.log" 2>&1
rm "$tmp/conformance/tests/clean.lint"

if run_gate > "$tmp/warning.log" 2>&1; then
  echo "lint-harn passed a current-format warning diagnostic" >&2
  exit 1
fi
grep -q 'HARN-LNT-999' "$tmp/warning.log"

echo "lint-harn gate regression checks passed"
