#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
stdlib_root="${HARN_STDLIB_HOST_NEUTRAL_ROOT:-$repo_root/crates/harn-stdlib/src/stdlib}"
baseline="${HARN_STDLIB_HOST_NEUTRAL_BASELINE:-$repo_root/scripts/stdlib-host-specific-compatibility.txt}"

if [[ ! -d "$stdlib_root" ]]; then
  echo "error: embedded stdlib root does not exist: $stdlib_root" >&2
  exit 2
fi
if [[ ! -f "$baseline" ]]; then
  echo "error: stdlib host-specific compatibility baseline does not exist: $baseline" >&2
  exit 2
fi

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/harn-stdlib-host-neutral.XXXXXX")"
trap 'rm -rf "$tmp_dir"' EXIT

set +e
(
  cd "$stdlib_root"
  grep -R -n -i -E \
    --include='*.harn' --include='*.md' --include='*.rs' \
    '(^|[^[:alnum:]_])burin(-code)?([^[:alnum:]_]|$)|\.burin|burin_' .
) >"$tmp_dir/actual-with-lines.txt"
scan_status=$?
set -e
if [[ "$scan_status" -gt 1 ]]; then
  echo "error: failed to scan embedded stdlib for host-specific names" >&2
  exit "$scan_status"
fi

sed -E 's#^([^:]+):[0-9]+:#\1:#' "$tmp_dir/actual-with-lines.txt" \
  | LC_ALL=C sort >"$tmp_dir/actual.txt"
sed '/^[[:space:]]*#/d; /^[[:space:]]*$/d' "$baseline" \
  | LC_ALL=C sort >"$tmp_dir/expected.txt"

if ! diff -u "$tmp_dir/expected.txt" "$tmp_dir/actual.txt"; then
  echo >&2
  echo "error: embedded stdlib contains an unreviewed host-specific name" >&2
  echo "Rewrite generic behavior in host-neutral terms. Retain a literal only" >&2
  echo "for an explicit compatibility adapter or useful public provenance," >&2
  echo "and record that ownership decision in $baseline." >&2
  exit 1
fi

echo "embedded stdlib host-neutral scan passed"
