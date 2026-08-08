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
  echo "missing tree-sitter CLI at $CLI_BIN" >&2
  echo "run \`cd tree-sitter-harn && npm ci\` before grammar build/test commands" >&2
  exit 1
fi

if [[ "${1:-}" == "test" ]]; then
  shift
  # `tree-sitter test` otherwise reuses its ambient compiled-language cache.
  # Pinning this repository's grammar path forces a rebuild, so corpus results
  # always prove the checked-in parser rather than whichever library ran last.
  exec "$CLI_BIN" test --grammar-path "$GRAMMAR_DIR" "$@"
fi

exec "$CLI_BIN" "$@"
