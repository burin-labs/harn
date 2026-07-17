#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
fixture_root=${CONFORMANCE_LINT_ROOT:-"$repo_root/conformance/tests"}
baseline=${CONFORMANCE_LINT_BASELINE:-"$repo_root/conformance/lint-baseline.tsv"}
harn_bin=${HARN_BIN:?HARN_BIN must name the harn binary under test}
workers=${CONFORMANCE_LINT_JOBS:-8}
tmp=$(mktemp -d "${TMPDIR:-/tmp}/harn-conformance-lint.XXXXXX")
trap 'rm -rf "$tmp"' EXIT HUP INT TERM

# The single-quoted program is intentionally evaluated by each worker shell.
# shellcheck disable=SC2016
find "$fixture_root" -name '*.harn' -print0 |
  while IFS= read -r -d '' file; do
    [[ -f "${file%.harn}.error" || -f "${file%.harn}.lint" ]] || printf '%s\0' "$file"
  done |
  xargs -0 -n 8 -P "$workers" bash -c '
    harn_bin=$1
    fixture_root=$2
    shift 2
    for file in "$@"; do
      set +e
      output=$("$harn_bin" check "$file" 2>&1)
      status=$?
      set -e
      relative=${file#"$fixture_root"/}
      while IFS= read -r line; do
        if [[ $line =~ ^(warning|error)\[([^]]+)\] ]]; then
          printf "%s\t%s\n" "$relative" "${BASH_REMATCH[2]}"
        fi
      done <<< "$output"
      if (( status != 0 )); then
        printf "%s\t%s\n" "$relative" "__CHECK_FAILED__"
      fi
    done
  ' _ "$harn_bin" "$fixture_root" > "$tmp/raw.tsv"

LC_ALL=C sort "$tmp/raw.tsv" | uniq -c |
  sed -E 's/^ *([0-9]+) /\1\t/' > "$tmp/actual.tsv"

if [[ ${1:-} == "--update" ]]; then
  cp "$tmp/actual.tsv" "$baseline"
  printf 'Updated %s\n' "$baseline"
  exit 0
fi

if ! diff -u "$baseline" "$tmp/actual.tsv"; then
  printf '%s\n' \
    'Conformance diagnostics changed. Fix the finding or explicitly review and regenerate:' \
    '  HARN_BIN=<path> ./scripts/check-conformance-lint-baseline.sh --update' >&2
  exit 1
fi

printf 'Conformance lint baseline matches (%s reviewed rows).\n' "$(wc -l < "$baseline" | tr -d ' ')"
