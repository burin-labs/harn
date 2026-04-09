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

CLI_BIN="$GRAMMAR_DIR/node_modules/.bin/tree-sitter"

if [[ ! -x "$CLI_BIN" ]]; then
  echo "bootstrapping tree-sitter-harn npm dependencies" >&2
  if [[ -f "$GRAMMAR_DIR/package-lock.json" ]]; then
    (cd "$GRAMMAR_DIR" && npm ci)
  else
    (cd "$GRAMMAR_DIR" && npm install)
  fi
fi

exec "$CLI_BIN" "$@"
