#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
grammar_dir="${TREE_SITTER_HARN_DIR:-$repo_root/tree-sitter-harn}"
cli="$grammar_dir/node_modules/.bin/tree-sitter"

usage() {
  echo "usage: $0 --write|--check" >&2
  exit 2
}

verify_pinned_cli() {
  if [[ ! -x "$cli" ]]; then
    echo "missing pinned tree-sitter CLI at $cli" >&2
    echo "run \`cd tree-sitter-harn && npm ci\` first" >&2
    exit 1
  fi

  local expected actual
  expected="$(
    cd "$grammar_dir"
    node -p "require('./package-lock.json').packages['node_modules/tree-sitter-cli'].version"
  )"
  actual="$("$cli" --version | awk '{print $2}')"
  if [[ -z "$expected" || "$actual" != "$expected" ]]; then
    echo "tree-sitter CLI version mismatch: package-lock.json pins $expected, installed CLI is $actual" >&2
    echo "run \`cd tree-sitter-harn && npm ci\` first" >&2
    exit 1
  fi
}

generate() {
  (
    cd "$1"
    ./scripts/tree-sitter-cli.sh generate
  )
}

prepare_isolated_grammar() {
  local isolated="$1"
  mkdir -p "$isolated/src"

  cp "$grammar_dir/grammar.js" \
    "$grammar_dir/package.json" \
    "$grammar_dir/package-lock.json" \
    "$grammar_dir/tree-sitter.json" \
    "$isolated/"
  cp -R "$grammar_dir/grammar" "$grammar_dir/scripts" "$isolated/"

  # scanner.c is hand-written, but shares src/ with the CLI's generated tree.
  # Seed it so the complete src/ directories can be compared structurally.
  cp "$grammar_dir/src/scanner.c" "$isolated/src/"
  ln -s "$grammar_dir/node_modules" "$isolated/node_modules"
}

find_src_files() {
  (
    cd "$1"
    find src -type f -print
  )
}

[[ $# -eq 1 ]] || usage
verify_pinned_cli

case "$1" in
  --write)
    generate "$grammar_dir"
    ;;
  --check)
    tmp_root="$(mktemp -d "${TMPDIR:-/tmp}/harn-tree-sitter-generated.XXXXXX")"
    trap 'rm -rf "$tmp_root"' EXIT
    isolated="$tmp_root/tree-sitter-harn"
    prepare_isolated_grammar "$isolated"

    generate "$isolated"

    candidates="$tmp_root/src-files"
    {
      find_src_files "$grammar_dir"
      find_src_files "$isolated"
    } | LC_ALL=C sort -u >"$candidates"

    stale=()
    while IFS= read -r output; do
      if ! cmp -s "$grammar_dir/$output" "$isolated/$output"; then
        stale+=("$output")
      fi
    done <"$candidates"

    if [[ ${#stale[@]} -ne 0 ]]; then
      echo "tree-sitter parser generated artifacts are stale:" >&2
      printf '  tree-sitter-harn/%s\n' "${stale[@]}" >&2
      echo "run \`make gen-tree-sitter-parser\` and commit the results" >&2
      exit 1
    fi

    echo "tree-sitter parser generated artifacts are current"
    ;;
  *)
    usage
    ;;
esac
