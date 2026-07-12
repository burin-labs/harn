#!/bin/sh

HOOK_RUST_PATTERN='(^Cargo\.toml$|^Cargo\.lock$|\.rs$|^crates/[^/]+/Cargo\.toml$)'
HOOK_TEST_PATTERN='(^Cargo\.toml$|^Cargo\.lock$|\.rs$|\.harn$|^crates/|^conformance/|^experiments/|^scripts/)'
HOOK_HARN_PATTERN='(\.harn$|^conformance/tests/|^experiments/)'
HOOK_MARKDOWN_PATTERN='\.md$'
HOOK_ACTIONS_PATTERN='(^\.github/workflows/|^\.githooks/|^Makefile$)'
HOOK_PORTAL_PATTERN='(^crates/harn-cli/portal/|^package(-lock)?\.json$)'
HOOK_HIGHLIGHT_CORE_PATTERN='(^crates/harn-lexer/|^crates/harn-stdlib/src/lib\.rs$|^crates/harn-vm/src/(stdlib|lib\.rs)|^crates/harn-modules/|^crates/harn-cli/src/commands/dump_highlight_keywords\.rs$|^crates/harn-cli/src/commands/portal/highlight\.rs$|^docs/theme/harn-keywords\.js$)'
HOOK_HIGHLIGHT_ENTRYPOINT_MARKER='@harn-entrypoint-category'
HOOK_LANGSPEC_PATTERN='(^spec/chapters/.*\.md$|^spec/HARN_SPEC\.md$|^docs/src/language-spec\.md$|^docs/src/spec/language/.*\.md$|^docs/src/SUMMARY\.md$)'
HOOK_DIAGCATALOG_PATTERN='(^crates/harn-parser/src/diagnostic_codes(\.rs|/)|^docs/src/diagnostics\.md$|^docs/diagnostics-catalog\.json$)'
HOOK_SESSION_BUNDLE_SCHEMA_PATTERN='(^crates/harn-vm/src/session_bundle\.rs$|^crates/harn-vm/src/session_bundle/|^crates/harn-cli/src/commands/session\.rs$|^spec/schemas/session-bundle\.v1\.schema\.json$)'
HOOK_PROMPT_PROSE_PATTERN='(^crates/harn-vm/src/(llm/|orchestration/(workflow|artifacts|compaction)\.rs$)|^conformance/|^scripts/(allowed_long_strings\.txt|check_no_rust_prompt_prose\.sh|check_rust_prompt_prose\.harn)$)'
HOOK_XFAIL_RATCHET_PATTERN='(^conformance/|^scripts/(check_xfail_count\.harn|xfail_threshold\.txt)$)'
HOOK_RATCHET_PATTERN="($HOOK_PROMPT_PROSE_PATTERN|$HOOK_XFAIL_RATCHET_PATTERN)"
# Lexer KEYWORDS const <-> tree-sitter keyword mirror.
HOOK_TREESITTER_PATTERN='(^crates/harn-lexer/src/token\.rs$|^tree-sitter-harn/grammar/keywords\.js$)'
# The generated-artifact registry and the consumers its audit cross-checks:
# Makefile target/recipe lists, the CI workflow that references generated
# artifact checks, and the hook logic. Other workflows (for example release
# binary packaging) do not change generated-artifact coverage. When the guard
# does run, it must use a fresh worktree Harn binary because registry coverage
# is computed by Harn source from this checkout.
HOOK_GENREGISTRY_PATTERN='(^scripts/generated_artifacts\.toml$|^scripts/check_generated_registry\.harn$|^Makefile$|^\.github/workflows/ci\.yml$|^\.githooks/)'
HOOK_HARN_FORMAT_SKIP=' semicolon_statements.harn semicolon_if_else_invalid.harn semicolon_try_catch_invalid.harn semicolon_empty_statement_invalid.harn '

hook_paths_match() {
  file_list=$1
  pattern=$2
  [ -s "$file_list" ] && grep -Eq "$pattern" "$file_list"
}

hook_paths_need_highlight() {
  file_list=$1
  shift

  if hook_paths_match "$file_list" "$HOOK_HIGHLIGHT_CORE_PATTERN"; then
    return 0
  fi

  [ -s "$file_list" ] || return 1
  while IFS= read -r path; do
    case "$path" in
      crates/harn-stdlib/src/stdlib/*.harn)
        # Imported stdlib modules are not highlighted as globals. Only Harn
        # entrypoint modules are registered as callable VM builtins whose
        # exported names can change docs/portal highlighting.
        if [ -f "$path" ] && grep -Fq "$HOOK_HIGHLIGHT_ENTRYPOINT_MARKER" "$path"; then
          return 0
        fi
        if [ "$#" -gt 0 ] && git diff "$@" -- "$path" 2>/dev/null | grep -Fq "$HOOK_HIGHLIGHT_ENTRYPOINT_MARKER"; then
          return 0
        fi
        ;;
    esac
  done < "$file_list"

  return 1
}

hook_no_local_build_mode() {
  [ "${HARN_HOOKS_NO_LOCAL_BUILD:-0}" = "1" ] || [ "${HARN_HOOKS_FAST_ONLY:-0}" = "1" ]
}

hook_skip_no_local_build() {
  label=$1
  if hook_no_local_build_mode; then
    echo "=== Hook: skipping $label (HARN_HOOKS_NO_LOCAL_BUILD/HARN_HOOKS_FAST_ONLY; remote CI remains required) ==="
    return 0
  fi
  return 1
}

# ---------------------------------------------------------------------------
# Hook duration instrument. Appends one NDJSON line per hook invocation to
# ~/.burin/hook-timings.ndjson: {ts, repo, hook, duration_ms, exit_code,
# commit_sha, host}. Zero-dep (POSIX sh + date only), never changes the
# hook's own exit code, and degrades silently if the log directory can't be
# created (e.g. no ~/.burin on this machine). Usage from a hook script:
#
#   . "$hook_dir/lib.sh"
#   hook_timing_start pre-commit
#   trap 'hook_timing_finish $?' EXIT
#   ...rest of the hook...
#
# The trap re-propagates $? verbatim on every exit path, including `set -e`
# early exits, so this can wrap hooks (like pre-commit/pre-push here) that
# rely on `set -e` failing fast partway through.
# ---------------------------------------------------------------------------
HOOK_TIMING_LOG_DIR="${HOOK_TIMING_LOG_DIR:-$HOME/.burin}"
HOOK_TIMING_LOG_FILE="$HOOK_TIMING_LOG_DIR/hook-timings.ndjson"

# Nanoseconds-since-epoch as a plain integer. Both GNU date and BSD/macOS
# date support `+%s%N` (BSD date zero-pads %N to 9 digits even though its
# actual clock resolution is coarser), so this is portable across the
# platforms this repo targets. Falls back to seconds-only (x10^9) if `%N`
# ever comes back literally as "N" (some minimal `date` builds).
hook_timing_now_ns() {
  raw=$(date +%s%N 2>/dev/null) || raw=""
  case "$raw" in
    *N) raw="$(date +%s)000000000" ;;
    "") raw="0" ;;
  esac
  printf '%s' "$raw"
}

# Call at the very top of a hook, right after sourcing lib.sh.
#   $1 = hook name, e.g. "pre-commit" or "pre-push"
hook_timing_start() {
  HOOK_TIMING_HOOK_NAME=$1
  HOOK_TIMING_START_NS=$(hook_timing_now_ns)
}

# Call via `trap 'hook_timing_finish $?' EXIT` immediately after
# hook_timing_start. Re-exits with the same code so the trap never masks or
# changes the hook's real outcome.
hook_timing_finish() {
  exit_code=$1
  (
    end_ns=$(hook_timing_now_ns)
    start_ns="${HOOK_TIMING_START_NS:-$end_ns}"
    duration_ms=$(( (end_ns - start_ns) / 1000000 ))
    [ "$duration_ms" -lt 0 ] 2>/dev/null && duration_ms=0

    repo_root=$(git rev-parse --show-toplevel 2>/dev/null) || repo_root=""
    repo=$(basename "${repo_root:-unknown}")
    commit_sha=$(git rev-parse HEAD 2>/dev/null) || commit_sha="unknown"
    host=$(hostname -s 2>/dev/null || hostname 2>/dev/null || echo unknown)
    ts=$(date -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo "")

    mkdir -p "$HOOK_TIMING_LOG_DIR" 2>/dev/null || exit 0
    [ -d "$HOOK_TIMING_LOG_DIR" ] || exit 0

    printf '{"ts":"%s","repo":"%s","hook":"%s","duration_ms":%s,"exit_code":%s,"commit_sha":"%s","host":"%s"}\n' \
      "$ts" "$repo" "${HOOK_TIMING_HOOK_NAME:-unknown}" "$duration_ms" "$exit_code" "$commit_sha" "$host" \
      >> "$HOOK_TIMING_LOG_FILE" 2>/dev/null || true
  ) || true
  exit "$exit_code"
}

# Resolve the workspace target dir. Used by hook_run_harn and
# scripts/sign_local_macos.sh to find freshly-built binaries after
# `cargo build`.
hook_target_dir() {
  if [ -n "${HARN_DEV_TARGET_DIR:-}" ]; then
    printf '%s\n' "$HARN_DEV_TARGET_DIR"
    return
  fi
  if [ -n "${CARGO_TARGET_DIR:-}" ]; then
    printf '%s\n' "$CARGO_TARGET_DIR"
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

hook_default_target_dir() {
  repo_root=$(git rev-parse --show-toplevel 2>/dev/null || pwd -P)
  repo_leaf=$(basename "$repo_root")
  repo_parent=$(basename "$(dirname "$repo_root")")
  printf '%s/harn-target/%s-%s\n' "${TMPDIR:-/tmp}" "$repo_parent" "$repo_leaf"
}

hook_export_cargo_build_dir() {
  if [ -n "${CARGO_BUILD_BUILD_DIR:-}" ]; then
    return 0
  fi

  repo_root=$(git rev-parse --show-toplevel 2>/dev/null || pwd -P)
  if [ -f "$repo_root/scripts/lib/cargo_env.sh" ]; then
    # shellcheck source=/dev/null
    . "$repo_root/scripts/lib/cargo_env.sh"
    if harn_export_cargo_build_dir_under_target "$CARGO_TARGET_DIR"; then
      printf '=== Hook: using Cargo build dir %s ===\n' "$CARGO_BUILD_BUILD_DIR" >&2
    fi
    return 0
  fi

  CARGO_BUILD_BUILD_DIR="$CARGO_TARGET_DIR/build"
  export CARGO_BUILD_BUILD_DIR
  printf '=== Hook: using Cargo build dir %s ===\n' "$CARGO_BUILD_BUILD_DIR" >&2
}

hook_export_cargo_target_dir() {
  if [ -z "${CARGO_TARGET_DIR:-}" ]; then
    target_dir=$(hook_target_dir)
    if [ -z "$target_dir" ]; then
      target_dir=$(hook_default_target_dir)
    fi
    export CARGO_TARGET_DIR="$target_dir"
    printf '=== Hook: using Cargo target dir %s ===\n' "$CARGO_TARGET_DIR" >&2
  fi
  mkdir -p "$CARGO_TARGET_DIR"
  hook_export_cargo_build_dir
}

hook_build_harn_bin() {
  RUSTC_WRAPPER= CARGO_BUILD_RUSTC_WRAPPER= cargo build --quiet --bin harn
}

# Build the workspace `harn` binary and re-apply the local codesign so
# the freshly re-linked binary keeps its ad-hoc signature — otherwise
# Gatekeeper shows a multi-second "Verifying 'harn'..." popup the first
# time the hook execs it. Echoes the path to the signed binary on
# stdout so hooks can invoke it directly (or via `xargs`); progress
# output is routed to stderr so command substitution stays clean.
# Idempotent; safe to call once per hook invocation.
hook_ensure_harn() {
  if [ -n "${HARN_BIN:-}" ]; then
    if [ ! -x "$HARN_BIN" ]; then
      echo "HARN_BIN is not executable: $HARN_BIN" >&2
      exit 1
    fi
    printf '%s\n' "$HARN_BIN"
    return
  fi

  hook_export_cargo_target_dir
  hook_build_harn_bin >&2
  if [ "$(uname)" = "Darwin" ] && [ -x "scripts/sign_local_macos.sh" ]; then
    HARN_LOCAL_SIGN_QUIET=1 ./scripts/sign_local_macos.sh >&2
  fi
  target_dir=$(hook_target_dir)
  target_dir=${target_dir:-target}
  printf '%s\n' "$target_dir/debug/harn"
}

hook_export_harn_bin() {
  if [ -n "${HARN_BIN:-}" ]; then
    if [ ! -x "$HARN_BIN" ]; then
      echo "HARN_BIN is not executable: $HARN_BIN" >&2
      exit 1
    fi
    export HARN_BIN
    return 0
  fi
  HARN_BIN=$(hook_ensure_harn)
  export HARN_BIN
}

hook_export_fresh_worktree_harn_bin() {
  HARN_BIN=$(unset HARN_BIN; ./scripts/harn_bin.sh --print)
  export HARN_BIN
}

hook_export_existing_harn_bin_for_non_rust_changes() {
  changed_file_list=$1
  if hook_paths_match "$changed_file_list" "$HOOK_RUST_PATTERN"; then
    return 0
  fi
  if [ -n "${HARN_BIN:-}" ]; then
    hook_export_harn_bin
    return 0
  fi
  path_harn=$(command -v harn 2>/dev/null || true)
  if [ -z "$path_harn" ]; then
    return 0
  fi
  HARN_BIN=$path_harn
  export HARN_BIN
  printf '=== Hook: reusing HARN_BIN %s (no Rust/Cargo changes) ===\n' "$HARN_BIN" >&2
}

hook_write_staged_files() {
  git diff --cached --name-only --diff-filter=ACMR > "$1"
}

hook_push_base() {
  upstream=$(git rev-parse --abbrev-ref --symbolic-full-name '@{upstream}' 2>/dev/null || true)
  if [ -n "$upstream" ]; then
    git merge-base HEAD "$upstream"
  elif git rev-parse --verify origin/main >/dev/null 2>&1; then
    git merge-base HEAD origin/main
  else
    git rev-list --max-parents=0 HEAD | tail -n 1
  fi
}

hook_validation_base() {
  if [ -n "${HARN_PREPUSH_VALIDATION_BASE:-}" ]; then
    base=$(git merge-base HEAD "$HARN_PREPUSH_VALIDATION_BASE" 2>/dev/null || true)
    if [ -n "$base" ]; then
      printf '%s\n' "$base"
      return 0
    fi
  fi

  branch=$(git rev-parse --abbrev-ref HEAD 2>/dev/null || true)
  if [ -n "$branch" ] && [ "$branch" != "HEAD" ] && [ "$branch" != "main" ] && \
     git rev-parse --verify origin/main >/dev/null 2>&1; then
    base=$(git merge-base HEAD origin/main 2>/dev/null || true)
    if [ -n "$base" ]; then
      printf '%s\n' "$base"
      return 0
    fi
  fi

  hook_push_base
}

hook_write_push_files() {
  output=$1
  base=${2:-$(hook_validation_base)}
  git diff --name-only --diff-filter=ACMR "$base"...HEAD > "$output"
}

# Cover the same incremental-cache corruption that scripts/release_gate.sh
# `cmd_prepare` and scripts/release_ship.sh `prepare_here` work around for
# their own cargo invocations: once `bump_version` rewrites Cargo.toml the
# workspace crates rebuild with fresh hashes, and any
# `target/debug/incremental/` populated against the previous version
# leaves dangling .o references that abort cargo with "failed to open
# object file ... No such file or directory" / "extern location for
# harn_modules does not exist". Those scripts export CARGO_INCREMENTAL=0
# in their own process, but git hooks run cargo from fresh subprocesses
# the export does not reach. Mirror the fix here for the hook context.
#
# Day-to-day commits keep incremental cache enabled — we only disable it
# when the staged or push diff is actually bumping the workspace
# version line.
#
# Args:
#   $1 = "staged"        # pre-commit context (uses --cached)
#      | "push <base>"   # pre-push context (uses <base>...HEAD)
hook_disable_cargo_incremental_if_release_bump() {
  case "$1" in
    staged)
      diff_args="--cached"
      ;;
    "push "*)
      base=${1#push }
      diff_args="$base...HEAD"
      ;;
    *)
      return 0
      ;;
  esac
  # `version =` matches both the root workspace key and per-crate
  # manifests; either one moving is a workspace bump signal.
  # shellcheck disable=SC2086
  if git diff $diff_args -- Cargo.toml 'crates/*/Cargo.toml' 2>/dev/null \
      | grep -Eq '^[-+]version = '; then
    echo "=== Hook: workspace version bump detected; disabling cargo incremental cache ==="
    export CARGO_INCREMENTAL=0
    target_dir=$(hook_target_dir)
    target_dir=${target_dir:-target}
    rm -rf "$target_dir/debug/incremental"
  fi
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
          # Skip crates listed in the workspace `exclude = [...]` line.
          # `cargo check -p <name>` cannot target them, so gating the
          # push on those crates produces a "did not match any packages"
          # error. Match against the workspace's exclude line directly
          # rather than parsing all the way to a `]` (the harn workspace
          # writes it inline on a single line).
          if [ -f "Cargo.toml" ] && \
             grep -E '^exclude *= *\[' Cargo.toml \
               | grep -Fq "\"crates/$crate\""; then
            continue
          fi
          package=$(awk -F '"' '/^name = / { print $2; exit }' "$manifest")
          [ -n "$package" ] && printf '%s\n' "$package" >> "$output"
        fi
        ;;
    esac
  done < "$input"
  sort -u -o "$output" "$output"
}
