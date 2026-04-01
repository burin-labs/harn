#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="$ROOT_DIR/spec/HARN_SPEC.md"
DST="$ROOT_DIR/docs/src/language-spec.md"

if [[ ! -f "$SRC" ]]; then
  echo "error: missing $SRC" >&2
  exit 1
fi

{
  echo "<!-- Generated from spec/HARN_SPEC.md by scripts/sync_language_spec.sh -->"
  echo ""
  cat "$SRC"
} >"$DST"
