#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="$ROOT_DIR/spec/HARN_SPEC.md"
DST="$ROOT_DIR/docs/src/language-spec.md"
MODE="${1:-write}"

if [[ ! -f "$SRC" ]]; then
  echo "error: missing $SRC" >&2
  exit 1
fi

emit_spec() {
  echo "<!-- Generated from spec/HARN_SPEC.md by scripts/sync_language_spec.sh -->"
  echo ""
  cat "$SRC"
}

case "$MODE" in
  --check)
    TMP="$(mktemp)"
    trap 'rm -f "$TMP"' EXIT
    emit_spec >"$TMP"
    if ! python3 "$ROOT_DIR/scripts/compare_generated_text.py" "$DST" "$TMP"; then
      echo "error: docs/src/language-spec.md is stale relative to spec/HARN_SPEC.md" >&2
      echo "hint: run 'make sync-language-spec' and commit the result" >&2
      exit 1
    fi
    ;;
  write)
    emit_spec >"$DST"
    ;;
  *)
    echo "usage: $0 [--check]" >&2
    exit 2
    ;;
esac
