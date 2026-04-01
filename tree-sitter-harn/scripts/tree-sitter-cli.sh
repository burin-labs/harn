#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
GRAMMAR_DIR="$(cd -- "$SCRIPT_DIR/.." && pwd)"
CONFIG_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/harn-tree-sitter-config.XXXXXX")"
CONFIG_DIR="$CONFIG_ROOT/tree-sitter"
CONFIG_FILE="$CONFIG_DIR/config.json"

cleanup() {
  rm -rf "$CONFIG_ROOT"
}

trap cleanup EXIT

mkdir -p "$CONFIG_DIR"

cat >"$CONFIG_FILE" <<EOF
{
  "parser-directories": [
    "$GRAMMAR_DIR"
  ]
}
EOF

export XDG_CONFIG_HOME="$CONFIG_ROOT"

exec "$GRAMMAR_DIR/node_modules/.bin/tree-sitter" "$@"
