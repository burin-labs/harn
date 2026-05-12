#!/bin/sh

HOOK_RUST_PATTERN='(^Cargo\.toml$|^Cargo\.lock$|\.rs$|^crates/[^/]+/Cargo\.toml$|^Makefile$)'
HOOK_TEST_PATTERN='(^Cargo\.toml$|^Cargo\.lock$|\.rs$|\.harn$|^crates/|^conformance/|^experiments/|^Makefile$|^scripts/)'
HOOK_HARN_PATTERN='(\.harn$|^conformance/tests/|^experiments/)'
HOOK_MARKDOWN_PATTERN='\.md$'
HOOK_ACTIONS_PATTERN='(^\.github/workflows/|^\.githooks/|^Makefile$)'
HOOK_PORTAL_PATTERN='(^crates/harn-cli/portal/|^package(-lock)?\.json$)'
HOOK_HIGHLIGHT_PATTERN='(^crates/harn-lexer/|^crates/harn-stdlib/src/stdlib/|^crates/harn-vm/src/(stdlib|lib\.rs)|^crates/harn-modules/|^docs/theme/harn-keywords\.js$)'
HOOK_LANGSPEC_PATTERN='(^spec/HARN_SPEC\.md$|^docs/src/language-spec\.md$)'
HOOK_RATCHET_PATTERN='(^crates/harn-vm/src/(llm/|orchestration/(workflow|artifacts|compaction)\.rs$)|^conformance/|^scripts/(allowed_long_strings\.txt|check_no_rust_prompt_prose\.sh|check_rust_prompt_prose\.py|check_xfail_count\.harn|xfail_threshold\.txt)$|^Makefile$)'
HOOK_HARN_FORMAT_SKIP=' semicolon_statements.harn semicolon_if_else_invalid.harn semicolon_try_catch_invalid.harn semicolon_empty_statement_invalid.harn '

hook_paths_match() {
  file_list=$1
  pattern=$2
  [ -s "$file_list" ] && grep -Eq "$pattern" "$file_list"
}

# Resolve the workspace target dir, matching the logic in
# scripts/sign_local_macos.sh. Used by hook_run_harn to find the
# freshly-built `harn` binary after `cargo build`.
hook_target_dir() {
  if [ -n "${HARN_DEV_TARGET_DIR:-}" ]; then
    printf '%s\n' "$HARN_DEV_TARGET_DIR"
    return
  fi
  if [ -f .cargo/config.toml ]; then
    awk -F'"' '
      /^\[build\][[:space:]]*$/ { in_build = 1; next }
      /^\[/ { in_build = 0 }
      in_build && /^[[:space:]]*target-dir[[:space:]]*=/ { print $2; exit }
    ' .cargo/config.toml
  fi
}

# Build the workspace `harn` binary and re-apply the local codesign so
# the freshly re-linked binary keeps its ad-hoc signature — otherwise
# Gatekeeper shows a multi-second "Verifying 'harn'..." popup the first
# time the hook execs it. Echoes the path to the signed binary on
# stdout so hooks can invoke it directly (or via `xargs`); progress
# output is routed to stderr so command substitution stays clean.
# Idempotent; safe to call once per hook invocation.
hook_ensure_harn() {
  cargo build --quiet --bin harn >&2
  if [ "$(uname)" = "Darwin" ] && [ -x "scripts/sign_local_macos.sh" ]; then
    HARN_LOCAL_SIGN_QUIET=1 ./scripts/sign_local_macos.sh >&2
  fi
  target_dir=$(hook_target_dir)
  target_dir=${target_dir:-target}
  printf '%s\n' "$target_dir/debug/harn"
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

hook_write_changed_cargo_packages() {
  input=$1
  output=$2
  : > "$output"
  while IFS= read -r path; do
    case "$path" in
      crates/*/*)
        crate=${path#crates/}
        crate=${crate%%/*}
        manifest="crates/$crate/Cargo.toml"
        if [ -f "$manifest" ]; then
          package=$(awk -F '"' '/^name = / { print $2; exit }' "$manifest")
          [ -n "$package" ] && printf '%s\n' "$package" >> "$output"
        fi
        ;;
    esac
  done < "$input"
  sort -u -o "$output" "$output"
}
