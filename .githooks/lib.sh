#!/bin/sh

HOOK_RUST_PATTERN='(^Cargo\.toml$|^Cargo\.lock$|\.rs$|^crates/[^/]+/Cargo\.toml$|^Makefile$)'
HOOK_TEST_PATTERN='(^Cargo\.toml$|^Cargo\.lock$|\.rs$|\.harn$|^crates/|^conformance/|^experiments/|^Makefile$|^scripts/)'
HOOK_HARN_PATTERN='(\.harn$|^conformance/tests/|^experiments/)'
HOOK_MARKDOWN_PATTERN='\.md$'
HOOK_ACTIONS_PATTERN='(^\.github/workflows/|^\.githooks/|^Makefile$)'
HOOK_PORTAL_PATTERN='(^crates/harn-cli/portal/|^package(-lock)?\.json$)'
HOOK_HIGHLIGHT_PATTERN='(^crates/harn-lexer/|^crates/harn-vm/src/(stdlib|stdlib_.*\.harn|lib\.rs)|^crates/harn-modules/|^docs/theme/harn-keywords\.js$)'
HOOK_LANGSPEC_PATTERN='(^spec/HARN_SPEC\.md$|^docs/src/language-spec\.md$)'
HOOK_HARN_FORMAT_SKIP=' semicolon_statements.harn semicolon_if_else_invalid.harn semicolon_try_catch_invalid.harn semicolon_empty_statement_invalid.harn '

hook_paths_match() {
  file_list=$1
  pattern=$2
  [ -s "$file_list" ] && grep -Eq "$pattern" "$file_list"
}

hook_write_staged_files() {
  git diff --cached --name-only --diff-filter=ACMR > "$1"
}

hook_write_push_files() {
  output=$1
  upstream=$(git rev-parse --abbrev-ref --symbolic-full-name '@{upstream}' 2>/dev/null || true)
  if [ -n "$upstream" ]; then
    base=$(git merge-base HEAD "$upstream")
  elif git rev-parse --verify origin/main >/dev/null 2>&1; then
    base=$(git merge-base HEAD origin/main)
  else
    base=$(git rev-list --max-parents=0 HEAD | tail -n 1)
  fi
  git diff --name-only --diff-filter=ACMR "$base"...HEAD > "$output"
}

hook_harn_format_supported() {
  harn_path=$1
  base=${harn_path##*/}
  case "$HOOK_HARN_FORMAT_SKIP" in
    *" $base "*) return 1 ;;
  esac
  [ ! -f "${harn_path%.harn}.error" ]
}

hook_harn_lint_supported() {
  harn_path=$1
  [ ! -f "${harn_path%.harn}.error" ]
}

hook_write_harn_format_files() {
  input=$1
  output=$2
  : > "$output"
  while IFS= read -r harn_path; do
    case "$harn_path" in
      *.harn)
        if [ -f "$harn_path" ] && hook_harn_format_supported "$harn_path"; then
          printf '%s\0' "$harn_path" >> "$output"
        fi
        ;;
    esac
  done < "$input"
}

hook_write_harn_lint_files() {
  input=$1
  output=$2
  : > "$output"
  while IFS= read -r harn_path; do
    case "$harn_path" in
      *.harn)
        if [ -f "$harn_path" ] && hook_harn_lint_supported "$harn_path"; then
          printf '%s\0' "$harn_path" >> "$output"
        fi
        ;;
    esac
  done < "$input"
}
