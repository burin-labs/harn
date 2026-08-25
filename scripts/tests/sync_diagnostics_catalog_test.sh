#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
tmp_root=$(mktemp -d)
cleanup() {
  local status=$?
  if [[ "$status" -ne 0 ]]; then
    for log in "$tmp_root"/*.log; do
      [[ -f "$log" ]] || continue
      printf '\n--- %s ---\n' "$(basename "$log")" >&2
      cat "$log" >&2
    done
  fi
  rm -rf "$tmp_root"
  exit "$status"
}
trap cleanup EXIT

fixture="$tmp_root/repo"
mkdir -p "$fixture/docs/src" "$fixture/bin"
printf 'old markdown\n' > "$fixture/docs/src/diagnostics.md"
printf '{"old":true}\n' > "$fixture/docs/diagnostics-catalog.json"

cat > "$fixture/bin/harn" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'call=%s\n' "$*" >> "$CATALOG_BINARY_RECORD"
format=''
while (( $# > 0 )); do
  if [[ "$1" = '--format' ]]; then
    format="${2:-}"
    break
  fi
  shift
done
if [[ "$format" = 'json' && "${CATALOG_FAIL_JSON:-0}" = '1' ]]; then
  echo 'forced JSON projection failure' >&2
  exit 23
fi
case "$format" in
  markdown) printf 'new markdown\n' ;;
  json) printf '{"new":true}\n' ;;
  *) echo "unexpected catalog format: $format" >&2; exit 2 ;;
esac
SH

cat > "$fixture/bin/resolve-harn" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'resolve=%s md=%s json=%s\n' \
  "$*" \
  "$(cat docs/src/diagnostics.md 2>/dev/null || printf __missing__)" \
  "$(cat docs/diagnostics-catalog.json 2>/dev/null || printf __missing__)" \
  >> "$CATALOG_RESOLVER_RECORD"
if [[ "${1:-}" = '--print' ]]; then
  printf '%s\n' "$CATALOG_FAKE_HARN"
  exit 0
fi
[[ "${1:-}" = '--' ]] && shift
exec "$CATALOG_FAKE_HARN" "$@"
SH
chmod +x "$fixture/bin/harn" "$fixture/bin/resolve-harn"

run_sync() {
  local output=$1
  shift
  (
    cd "$fixture"
    CATALOG_FAKE_HARN="$fixture/bin/harn" \
    CATALOG_RESOLVER_RECORD="$tmp_root/resolver.log" \
    CATALOG_BINARY_RECORD="$tmp_root/binary.log" \
      "$@" make --no-print-directory -f "$repo_root/Makefile" \
        HARN_BIN_PRINT_CMD="$fixture/bin/resolve-harn --print" \
        HARN_CMD="$fixture/bin/resolve-harn --" \
        sync-diagnostics-catalog
  ) > "$output" 2>&1
}

run_check() {
  local output=$1
  (
    cd "$fixture"
    CATALOG_FAKE_HARN="$fixture/bin/harn" \
    CATALOG_RESOLVER_RECORD="$tmp_root/resolver.log" \
    CATALOG_BINARY_RECORD="$tmp_root/binary.log" \
      make --no-print-directory -f "$repo_root/Makefile" \
        HARN_BIN_PRINT_CMD="$fixture/bin/resolve-harn --print" \
        HARN_CMD="$fixture/bin/resolve-harn --" \
        check-diagnostics-catalog
  ) > "$output" 2>&1
}

: > "$tmp_root/resolver.log"
: > "$tmp_root/binary.log"
run_sync "$tmp_root/success.log" env

if [[ "$(wc -l < "$tmp_root/resolver.log" | tr -d ' ')" -ne 1 ]]; then
  echo "catalog sync resolved the Harn binary more than once" >&2
  cat "$tmp_root/resolver.log" >&2
  exit 1
fi
if ! grep -Fq 'md=old markdown json={"old":true}' "$tmp_root/resolver.log"; then
  echo "catalog sync changed a published output before resolving its binary" >&2
  cat "$tmp_root/resolver.log" >&2
  exit 1
fi
if [[ "$(wc -l < "$tmp_root/binary.log" | tr -d ' ')" -ne 2 ]]; then
  echo "catalog sync did not project both formats from one resolved binary" >&2
  cat "$tmp_root/binary.log" >&2
  exit 1
fi
grep -Fxq 'new markdown' "$fixture/docs/src/diagnostics.md"
grep -Fxq '{"new":true}' "$fixture/docs/diagnostics-catalog.json"

: > "$tmp_root/resolver.log"
: > "$tmp_root/binary.log"
run_check "$tmp_root/check.log"
if [[ "$(wc -l < "$tmp_root/resolver.log" | tr -d ' ')" -ne 1 ]] || \
   [[ "$(wc -l < "$tmp_root/binary.log" | tr -d ' ')" -ne 2 ]]; then
  echo "catalog drift check did not reuse one resolved binary" >&2
  cat "$tmp_root/resolver.log" "$tmp_root/binary.log" >&2
  exit 1
fi

printf 'stable markdown\n' > "$fixture/docs/src/diagnostics.md"
printf '{"stable":true}\n' > "$fixture/docs/diagnostics-catalog.json"
: > "$tmp_root/resolver.log"
: > "$tmp_root/binary.log"
if run_sync "$tmp_root/failure.log" env CATALOG_FAIL_JSON=1; then
  echo "catalog sync ignored a failed JSON projection" >&2
  exit 1
fi
grep -Fxq 'stable markdown' "$fixture/docs/src/diagnostics.md"
grep -Fxq '{"stable":true}' "$fixture/docs/diagnostics-catalog.json"

echo "sync_diagnostics_catalog_test: ok"
