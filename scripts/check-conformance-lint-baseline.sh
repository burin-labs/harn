#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
fixture_root=${CONFORMANCE_LINT_ROOT:-"$repo_root/conformance/tests"}
baseline=${CONFORMANCE_LINT_BASELINE:-"$repo_root/conformance/lint-baseline.tsv"}
harn_bin=${HARN_BIN:?HARN_BIN must name the harn binary under test}
tmp=$(mktemp -d "${TMPDIR:-/tmp}/harn-conformance-lint.XXXXXX")
trap 'rm -rf "$tmp"' EXIT HUP INT TERM

files=()
hidden_files=()
while IFS= read -r -d '' file; do
  if [[ -f "${file%.harn}.error" || -f "${file%.harn}.lint" ]]; then
    continue
  fi
  if [[ ${file##*/} == .harn ]]; then
    hidden_files+=("$file")
  else
    files+=("$file")
  fi
# A conformance run creates state directories named `.harn`. They are not
# source files and should never become synthetic `__CHECK_FAILED__` baseline
# entries; restrict the lint corpus to actual files.
done < <(find "$fixture_root" -type f -name '*.harn' -print0)

: > "$tmp/raw.tsv"
batch_index=0
run_check_batch() {
  local -a requested=("$@")
  local report="$tmp/report-$batch_index.json"
  local stderr="$tmp/check-$batch_index.stderr"
  local requested_paths="$tmp/requested-$batch_index.txt"
  local reported_paths="$tmp/reported-$batch_index.txt"
  local missing_paths="$tmp/missing-$batch_index.txt"
  local unexpected_paths="$tmp/unexpected-$batch_index.txt"
  local file relative check_status error_count missing_count
  batch_index=$((batch_index + 1))

  ((${#requested[@]} > 0)) || return 0
  for file in "${requested[@]}"; do
    relative=${file#"$fixture_root"/}
    printf '%s\n' "$relative"
  done | LC_ALL=C sort > "$requested_paths"

  # One check process owns the whole tree and fans independent fixture graphs
  # through Harn's bounded native worker pool. The former shell loop paid CLI
  # startup and process scheduling 1,700+ times and made this CI gate the long
  # pole; --independent preserves its one-program-per-file semantics.
  set +e
  "$harn_bin" check --json --independent "${requested[@]}" > "$report" 2> "$stderr"
  check_status=$?
  set -e

  if jq -e '
      .schemaVersion == 1
      and (.data.files | type == "array")
      and all(.data.files[];
        (.path | type == "string")
        and (.status == "ok" or .status == "warning" or .status == "error")
        and (.diagnostics | type == "array"))
    ' "$report" >/dev/null 2>&1; then
    jq -r --arg root "$fixture_root/" '
      .data.files[] as $file
      | ($file.path | if startswith($root) then .[($root | length):] else . end) as $path
      | ($file.diagnostics[]?
          | select((.severity == "warning" or .severity == "error") and (.code? | type == "string"))
          | [$path, .code]
          | @tsv),
        (if $file.status == "error"
          then [$path, "__CHECK_FAILED__"] | @tsv
          else empty
          end)
    ' "$report" >> "$tmp/raw.tsv"

    jq -r --arg root "$fixture_root/" '
      .data.files[].path
      | if startswith($root) then .[($root | length):] else . end
    ' "$report" | LC_ALL=C sort -u > "$reported_paths"
    comm -23 "$requested_paths" "$reported_paths" > "$missing_paths"
    comm -13 "$requested_paths" "$reported_paths" > "$unexpected_paths"
    while IFS= read -r relative; do
      [[ -n $relative ]] && printf '%s\t%s\n' "$relative" '__CHECK_FAILED__' >> "$tmp/raw.tsv"
    done < "$missing_paths"

    error_count=$(jq '[.data.files[] | select(.status == "error")] | length' "$report")
    missing_count=$(wc -l < "$missing_paths" | tr -d ' ')
    if [[ -s $unexpected_paths ]] \
      || ((check_status != 0 && error_count + missing_count == 0)) \
      || ((check_status == 0 && error_count != 0)); then
      cat "$stderr" >&2
      printf '%s\t%s\n' '__BATCH__' '__CHECK_FAILED__' >> "$tmp/raw.tsv"
    fi
  elif ((check_status != 0)) \
    && jq -e '.schemaVersion == 1 and .data == null and (.error | type == "object")' \
      "$report" >/dev/null 2>&1; then
    # A real file literally named `.harn` is not a collected source target,
    # while a `.harn` directory is generated runtime state excluded above.
    while IFS= read -r relative; do
      [[ -n $relative ]] && printf '%s\t%s\n' "$relative" '__CHECK_FAILED__' >> "$tmp/raw.tsv"
    done < "$requested_paths"
  else
    cat "$stderr" >&2
    printf '%s\t%s\n' '__BATCH__' '__CHECK_FAILED__' >> "$tmp/raw.tsv"
  fi
}

run_check_batch "${files[@]}"
run_check_batch "${hidden_files[@]}"

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
