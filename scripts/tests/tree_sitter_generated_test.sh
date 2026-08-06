#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp_root="$(mktemp -d)"
trap 'rm -rf "$tmp_root"' EXIT
grammar="$tmp_root/tree-sitter-harn"
mkdir -p "$grammar/grammar" "$grammar/scripts" \
  "$grammar/node_modules/.bin"

cp "$repo_root/tree-sitter-harn/scripts/tree-sitter-cli.sh" "$grammar/scripts/"
printf 'module.exports = grammar({name: "fixture", rules: {source_file: $ => "ok"}});\n' \
  > "$grammar/grammar.js"
printf '{}\n' > "$grammar/package.json"
printf '{}\n' > "$grammar/tree-sitter.json"
cat > "$grammar/package-lock.json" <<'JSON'
{
  "packages": {
    "node_modules/tree-sitter-cli": {
      "version": "0.26.11"
    }
  }
}
JSON

cat > "$grammar/node_modules/.bin/tree-sitter" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == "--version" ]]; then
  echo "tree-sitter 0.26.11"
  exit 0
fi
[[ "${1:-}" == "generate" ]] || exit 2
mkdir -p src
source_text="$(cat grammar.js)"
printf 'parser:%s\n' "$source_text" > src/parser.c
printf 'grammar:%s\n' "$source_text" > src/grammar.json
printf 'nodes:%s\n' "$source_text" > src/node-types.json
SH
chmod +x "$grammar/node_modules/.bin/tree-sitter"

run_guard() {
  TREE_SITTER_HARN_DIR="$grammar" "$repo_root/scripts/tree_sitter_generated.sh" "$@"
}

run_guard --write
run_guard --check >/dev/null

outputs=(src/parser.c src/grammar.json src/node-types.json)
for output in "${outputs[@]}"; do
  printf 'stale\n' >> "$grammar/$output"
done
digest_outputs() {
  for output in "${outputs[@]}"; do
    cksum "$grammar/$output"
  done
}
before="$(digest_outputs)"

check_output="$tmp_root/check.out"
if run_guard --check >"$check_output" 2>&1; then
  echo "stale generated outputs unexpectedly passed" >&2
  exit 1
fi

for output in "${outputs[@]}"; do
  grep -Fq "tree-sitter-harn/$output" "$check_output" || {
    echo "failure did not name stale output $output" >&2
    exit 1
  }
done
grep -Fq "run \`make gen-tree-sitter-parser\` and commit the results" "$check_output" || {
  echo "failure did not provide the remediation command" >&2
  exit 1
}

after="$(digest_outputs)"
[[ "$before" == "$after" ]] || {
  echo "drift check mutated committed outputs" >&2
  exit 1
}

run_guard --write
run_guard --check >/dev/null
echo "tree_sitter_generated_test: ok"
