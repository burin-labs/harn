#!/bin/sh

HOOK_RUST_PATTERN='(^Cargo\.toml$|^Cargo\.lock$|\.rs$|^crates/[^/]+/Cargo\.toml$)'
# Paths that can change what a built binary emits. Broader than
# HOOK_RUST_PATTERN, which answers "is there Rust source to format/lint" —
# this answers "could the binary be stale". Crates compile non-Rust assets in
# via include_str!/include_bytes! (capability tables, diagnostic explanations,
# stdlib sources, bytecode, and tree-sitter queries), and editing one changes
# generated output without touching a single .rs file. Matching the production
# package roots leaves the which-files-matter question to cargo, which already
# tracks those includes as build dependencies; a path it does not consider an
# input rebuilds nothing.
HOOK_BINARY_INPUT_PATTERN='(^Cargo\.toml$|^Cargo\.lock$|\.rs$|^crates/|^tree-sitter-harn/)'
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
HOOK_PROVIDER_CATALOG_PATTERN='(^crates/harn-vm/src/llm/(catalog_sources/|capability_sources/|providers\.toml$|capabilities\.toml$)|^spec/provider-catalog/|^docs/src/provider-(matrix|support)\.md$|^docs/provider-support\.json$)'
# Lexer KEYWORDS const <-> tree-sitter keyword mirror.
HOOK_TREESITTER_PATTERN='(^crates/harn-lexer/src/token\.rs$|^tree-sitter-harn/grammar/keywords\.js$)'
# The generated-artifact registry and the consumers its audit cross-checks:
# Makefile target/recipe lists, the CI workflow that references generated
# artifact checks, and the hook logic. Other workflows (for example release
# binary packaging) do not change generated-artifact coverage. The guard
# executes Harn source from this checkout; only compiled Rust deltas require a
# fresh worktree runtime.
HOOK_GENREGISTRY_PATTERN='(^scripts/generated_artifacts\.toml$|^scripts/check_generated_registry\.harn$|^Makefile$|^\.github/workflows/ci\.yml$|^\.githooks/)'
HOOK_HARN_FORMAT_SKIP=' semicolon_statements.harn semicolon_if_else_invalid.harn semicolon_try_catch_invalid.harn semicolon_empty_statement_invalid.harn '

hook_paths_match() {
  file_list=$1
  pattern=$2
  [ -s "$file_list" ] && grep -Eq "$pattern" "$file_list"
}

hook_is_provider_catalog_data_path() {
  path=$1
  case "$path" in
    crates/harn-vm/src/llm/catalog_sources/*|\
    crates/harn-vm/src/llm/capability_sources/*|\
    crates/harn-vm/src/llm/providers.toml|\
    crates/harn-vm/src/llm/capabilities.toml|\
    spec/provider-catalog/*|\
    docs/src/provider-matrix.md|\
    docs/src/provider-support.md|\
    docs/provider-support.json)
      return 0
      ;;
  esac
  return 1
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

hook_fast_default_mode() {
  [ "${HARN_HOOKS_FULL_LOCAL:-0}" != "1" ]
}

hook_skip_no_local_build() {
  label=$1
  if hook_fast_default_mode; then
    echo "=== Hook: skipping $label (fast default; set HARN_HOOKS_FULL_LOCAL=1 to opt in; remote CI remains required) ==="
    return 0
  fi
  return 1
}

# ---------------------------------------------------------------------------
# Hook duration instrument. Appends one NDJSON line per hook invocation to
# ~/.burin/hook-timings.ndjson: {ts, repository, repo, hook, profile,
# duration_ms, phases, exit_code, commit_sha, host}. `repository` is the stable owner;
# legacy `repo` remains the checkout basename for worktree forensics. Zero-dep
# (POSIX sh + date only), never changes the
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
  HOOK_TIMING_PHASE_NAME=""
  HOOK_TIMING_PHASE_START_NS=""
  HOOK_TIMING_PHASES=""
}

# Close the active phase at a caller-supplied timestamp. Phase names are
# static hook-owned identifiers; rejecting anything else keeps the nested JSON
# safe without adding jq/Python to the commit path.
hook_timing_close_phase() {
  phase_end_ns=$1
  [ -n "${HOOK_TIMING_PHASE_NAME:-}" ] || return 0
  case "$HOOK_TIMING_PHASE_NAME" in
    *[!a-z0-9_-]*)
      HOOK_TIMING_PHASE_NAME=""
      HOOK_TIMING_PHASE_START_NS=""
      return 0
      ;;
  esac
  phase_start_ns="${HOOK_TIMING_PHASE_START_NS:-$phase_end_ns}"
  phase_duration_ms=$(( (phase_end_ns - phase_start_ns) / 1000000 ))
  [ "$phase_duration_ms" -lt 0 ] 2>/dev/null && phase_duration_ms=0
  if [ -n "${HOOK_TIMING_PHASES:-}" ]; then
    HOOK_TIMING_PHASES="$HOOK_TIMING_PHASES,"
  fi
  HOOK_TIMING_PHASES="$HOOK_TIMING_PHASES\"$HOOK_TIMING_PHASE_NAME\":$phase_duration_ms"
  HOOK_TIMING_PHASE_NAME=""
  HOOK_TIMING_PHASE_START_NS=""
}

# Transition from the current top-level phase to the next one. This records
# broad ownership boundaries, not every command, so hook control flow remains
# simple and timing overhead stays negligible.
hook_timing_phase() {
  phase_now_ns=$(hook_timing_now_ns)
  hook_timing_close_phase "$phase_now_ns"
  case "$1" in
    ""|*[!a-z0-9_-]*) return 0 ;;
  esac
  HOOK_TIMING_PHASE_NAME=$1
  HOOK_TIMING_PHASE_START_NS=$phase_now_ns
}

# Call via `trap 'hook_timing_finish $?' EXIT` immediately after
# hook_timing_start. Re-exits with the same code so the trap never masks or
# changes the hook's real outcome.
hook_timing_finish() {
  exit_code=$1
  (
    end_ns=$(hook_timing_now_ns)
    hook_timing_close_phase "$end_ns"
    start_ns="${HOOK_TIMING_START_NS:-$end_ns}"
    duration_ms=$(( (end_ns - start_ns) / 1000000 ))
    [ "$duration_ms" -lt 0 ] 2>/dev/null && duration_ms=0

    repo_root=$(git rev-parse --show-toplevel 2>/dev/null) || repo_root=""
    repo=$(basename "${repo_root:-unknown}")
    commit_sha=$(git rev-parse HEAD 2>/dev/null) || commit_sha="unknown"
    host=$(hostname -s 2>/dev/null || hostname 2>/dev/null || echo unknown)
    ts=$(date -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo "")
    if [ "${HARN_HOOKS_FULL_LOCAL:-0}" = "1" ]; then
      profile="full"
    else
      profile="fast"
    fi

    mkdir -p "$HOOK_TIMING_LOG_DIR" 2>/dev/null || exit 0
    [ -d "$HOOK_TIMING_LOG_DIR" ] || exit 0

    printf '{"ts":"%s","repository":"burin-labs/harn","repo":"%s","hook":"%s","profile":"%s","duration_ms":%s,"phases":{%s},"exit_code":%s,"commit_sha":"%s","host":"%s"}\n' \
      "$ts" "$repo" "${HOOK_TIMING_HOOK_NAME:-unknown}" "$profile" "$duration_ms" "${HOOK_TIMING_PHASES:-}" "$exit_code" "$commit_sha" "$host" \
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
  # shellcheck source=/dev/null
  . "$repo_root/scripts/lib/cargo_env.sh"
  if harn_export_cargo_build_dir_for_target "$CARGO_TARGET_DIR"; then
    printf '=== Hook: using Cargo build dir %s ===\n' "$CARGO_BUILD_BUILD_DIR" >&2
  fi
}

hook_export_freshness_target_dir() {
  if [ -z "${CARGO_TARGET_DIR:-}" ]; then
    target_dir=$(hook_target_dir)
    if [ -z "$target_dir" ]; then
      target_dir=$(hook_default_target_dir)
    fi
    export CARGO_TARGET_DIR="$target_dir"
    printf '=== Hook: using Cargo target dir %s ===\n' "$CARGO_TARGET_DIR" >&2
  fi
}

hook_export_cargo_target_dir() {
  hook_export_freshness_target_dir
  mkdir -p "$CARGO_TARGET_DIR"
  hook_export_cargo_build_dir
}

hook_build_harn_bin() {
  repo_root=$(git rev-parse --show-toplevel 2>/dev/null || pwd -P)
  HARN_BIN='' HARN_BIN_NO_BUILD=0 \
    RUSTC_WRAPPER='' RUSTC_WORKSPACE_WRAPPER='' \
    CARGO_BUILD_RUSTC_WRAPPER='' CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER='' \
    "$repo_root/scripts/harn_bin.sh" --print
}

hook_select_fresh_worktree_harn_bin() {
  # Git executes non-bare hooks from the worktree root. Keep the unchanged hot
  # path in-process instead of spawning Git merely to echo the current PWD.
  hook_fresh_repo_root=$PWD
  hook_fresh_target_dir=${CARGO_TARGET_DIR:-$(hook_target_dir)}
  hook_fresh_target_dir=${hook_fresh_target_dir:-target}
  hook_fresh_platform=${OS:-$(uname -s)}
  case "$hook_fresh_platform" in
    Windows_NT | MINGW* | MSYS* | CYGWIN*) hook_fresh_suffix=.exe ;;
    *) hook_fresh_suffix= ;;
  esac
  # Select the platform artifact structurally. Git Bash treats an existing
  # `harn.exe` as executable through the extensionless path `harn`, so probing
  # the suffixless name first loses the real filename and makes the checker
  # seek `harn.freshness` instead of the produced `harn.exe.freshness`.
  hook_fresh_bin="$hook_fresh_target_dir/debug/harn$hook_fresh_suffix"
  hook_fresh_checker="$hook_fresh_target_dir/debug/harn-freshness-check$hook_fresh_suffix"
  [ -x "$hook_fresh_bin" ] && [ -x "$hook_fresh_checker" ] || return 1
  "$hook_fresh_checker" verify-worktree \
    "$hook_fresh_bin" "$hook_fresh_repo_root" || return $?
  HARN_FRESH_WORKTREE_BIN=$hook_fresh_bin
}

hook_find_fresh_worktree_harn_bin() {
  hook_select_fresh_worktree_harn_bin || return $?
  printf '%s\n' "$HARN_FRESH_WORKTREE_BIN"
}

# Reuse an exactly proven worktree `harn`, or build and re-apply the local
# codesign so a freshly re-linked binary keeps its ad-hoc signature — otherwise
# Gatekeeper shows a multi-second "Verifying 'harn'..." popup the first time the
# hook execs it. Echoes the path on stdout so hooks can invoke it directly (or
# via `xargs`); progress output is routed to stderr so command substitution
# stays clean.
hook_ensure_harn() {
  if [ -n "${HARN_BIN:-}" ]; then
    if [ ! -x "$HARN_BIN" ]; then
      echo "HARN_BIN is not executable: $HARN_BIN" >&2
      exit 1
    fi
    printf '%s\n' "$HARN_BIN"
    return
  fi

  hook_export_freshness_target_dir
  # Exact receipt validation is the unchanged hook fast path. Only a missing or
  # stale proof falls through to Cargo; signing and receipt production belong
  # exclusively to that mutating build path.
  if hook_select_fresh_worktree_harn_bin 2>/dev/null; then
    printf '%s\n' "$HARN_FRESH_WORKTREE_BIN"
    return
  fi
  hook_export_cargo_target_dir
  resolved_harn=$(hook_build_harn_bin)
  if [ "$(uname)" = "Darwin" ] && [ -x "scripts/sign_local_macos.sh" ]; then
    HARN_LOCAL_SIGN_QUIET=1 ./scripts/sign_local_macos.sh >&2
    HARN_BIN='' ./scripts/harn_bin.sh --record-receipt >&2
  fi
  printf '%s\n' "$resolved_harn"
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
  HARN_BIN=$(unset HARN_BIN; hook_ensure_harn)
  export HARN_BIN
}

# Registry checks execute repository Harn source but only require a freshly
# built runtime when the pushed/staged delta changes an input to the binary.
# Changes that cannot affect it reuse the binary selected during hook
# initialization.
hook_export_registry_harn_bin() {
  changed_file_list=$1
  if hook_paths_match "$changed_file_list" "$HOOK_BINARY_INPUT_PATTERN"; then
    hook_export_fresh_worktree_harn_bin
  else
    hook_export_harn_bin
  fi
}

hook_check_generated_registry() {
  changed_file_list=$1
  hook_export_registry_harn_bin "$changed_file_list"
  "$HARN_BIN" run scripts/check_generated_registry.harn
}

# Find an already-built Harn without starting a build. The worktree resolver is
# authoritative because it understands lane-local Cargo target directories;
# PATH is only a last resort and may point at another checkout's changing
# binary.
hook_find_existing_harn_bin() {
  if [ -n "${HARN_BIN:-}" ]; then
    [ -x "$HARN_BIN" ] || return 1
    printf '%s\n' "$HARN_BIN"
    return 0
  fi

  repo_root=$(git rev-parse --show-toplevel 2>/dev/null || pwd -P)
  resolved=$(hook_find_fresh_worktree_harn_bin 2>/dev/null || true)
  if [ -n "$resolved" ] && [ -x "$resolved" ]; then
    printf '%s\n' "$resolved"
    return 0
  fi
  resolver="$repo_root/scripts/harn_bin.sh"
  if [ -x "$resolver" ]; then
    resolved=$(HARN_BIN_NO_BUILD=1 "$resolver" --no-build --print 2>/dev/null || true)
    if [ -n "$resolved" ] && [ -x "$resolved" ]; then
      printf '%s\n' "$resolved"
      return 0
    fi
  fi
  if [ -x "$repo_root/target/debug/harn" ]; then
    printf '%s\n' "$repo_root/target/debug/harn"
    return 0
  fi
  path_harn=$(command -v harn 2>/dev/null || true)
  if [ -n "$path_harn" ] && [ -x "$path_harn" ]; then
    printf '%s\n' "$path_harn"
    return 0
  fi
  return 1
}

# Warn (never fail) when staged paths touch a regenerable artifact's sources
# without staging any of its outputs. Path matching lives in
# scripts/warn_generated_artifact_drift.harn. Reuses an available harn binary
# without forcing a rebuild; skips quietly when none is available.
hook_warn_generated_artifact_drift() {
  changed_file_list=$1
  repo_root=$(git rev-parse --show-toplevel 2>/dev/null) || return 0
  script="$repo_root/scripts/warn_generated_artifact_drift.harn"
  [ -f "$script" ] || return 0

  harn_bin=$(hook_find_existing_harn_bin || true)
  if [ -z "$harn_bin" ]; then
    return 0
  fi

  # Pre-commit's staged list lives in system mktemp (outside the worktree).
  # Copy it under .harn/ so the default FS sandbox can read it — avoid
  # --no-sandbox so every commit does not print a sandbox-disable warning.
  mkdir -p "$repo_root/.harn/tmp"
  staged_in_repo=$(mktemp "$repo_root/.harn/tmp/staged-paths.XXXXXX") || return 0
  if ! cp "$changed_file_list" "$staged_in_repo" 2>/dev/null; then
    rm -f "$staged_in_repo"
    return 0
  fi
  # Stay advisory even if the script errors, but never turn a failed check into
  # a silent success. The required repository gate remains the final check.
  status=0
  "$harn_bin" run scripts/warn_generated_artifact_drift.harn -- \
    --staged-files "$staged_in_repo" || status=$?
  if [ "$status" -ne 0 ]; then
    echo "warning: generated-artifact drift check did not run (harn exited $status); the required repository gate will check it before merge" >&2
  fi
  rm -f "$staged_in_repo"
}

hook_export_existing_harn_bin_for_non_rust_changes() {
  changed_file_list=$1
  if hook_paths_match "$changed_file_list" "$HOOK_BINARY_INPUT_PATTERN"; then
    return 0
  fi
  if [ -n "${HARN_BIN:-}" ]; then
    hook_export_harn_bin
    return 0
  fi
  path_harn=$(hook_find_existing_harn_bin || true)
  if [ -z "$path_harn" ]; then
    return 0
  fi
  HARN_BIN=$path_harn
  export HARN_BIN
  printf '=== Hook: reusing HARN_BIN %s (no changes to binary inputs) ===\n' "$HARN_BIN" >&2
}

hook_write_staged_files() {
  git diff --cached --name-only --no-renames --diff-filter=ACMRD > "$1"
}

hook_write_staged_markdown_files() {
  git diff --cached --name-only -z --no-renames --diff-filter=ACMR -- '*.md' > "$1"
}

# Resolve the upstream branch name, but only when it names a real commit.
# `git rev-parse --abbrev-ref --symbolic-full-name @{upstream}` exits 128 and
# still prints the literal string `@{upstream}` on stdout when the branch has a
# configured upstream whose remote-tracking ref is gone — the normal state after
# a merged branch is auto-deleted on the remote. `|| true` swallows the exit
# code, so the caller would otherwise treat `@{upstream}` as a revision, and the
# `merge-base` against it fails under `set -e`. That aborted every push from
# such a branch, including deletions and refs that touch no local commits.
hook_resolved_upstream() {
  upstream=$(git rev-parse --abbrev-ref --symbolic-full-name '@{upstream}' 2>/dev/null || true)
  if [ -n "$upstream" ] && git rev-parse --verify --quiet "${upstream}^{commit}" >/dev/null 2>&1; then
    printf '%s\n' "$upstream"
  fi
}

hook_push_base() {
  upstream=$(hook_resolved_upstream)
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
  git diff --name-only --no-renames --diff-filter=ACMRD "$base"...HEAD > "$output"
}

hook_write_push_markdown_files() {
  output=$1
  base=${2:-$(hook_validation_base)}
  git diff --name-only -z --no-renames --diff-filter=ACMR "$base"...HEAD -- '*.md' > "$output"
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
  # A conformance fixture with a `.error` or `.lint` sibling asserts the
  # diagnostics it provokes, so linting it here is wrong by construction:
  # the fixture is *supposed* to be unclean, and `harn lint --fix` would
  # quietly rewrite away the very violation under test.
  [ ! -f "${harn_path%.harn}.error" ] && [ ! -f "${harn_path%.harn}.lint" ]
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

hook_cargo_package_for_path() {
  path=$1
  case "$path" in
    crates/*/*)
      crate=${path#crates/}
      crate=${crate%%/*}
      manifest="crates/$crate/Cargo.toml"
      [ -f "$manifest" ] || return 1
      # `cargo -p` cannot target excluded crates. Match the workspace's
      # single-line exclude list without pulling a TOML parser into hooks.
      if [ -f "Cargo.toml" ] && \
         grep -E '^exclude *= *\[' Cargo.toml \
           | grep -Fq "\"crates/$crate\""; then
        return 1
      fi
      package=$(awk -F '"' '/^name = / { print $2; exit }' "$manifest")
      [ -n "$package" ] || return 1
      printf '%s\n' "$package"
      return 0
      ;;
  esac
  return 1
}

hook_write_changed_cargo_packages() {
  input=$1
  output=$2
  : > "$output"
  while IFS= read -r path; do
    if hook_is_provider_catalog_data_path "$path"; then
      continue
    fi
    package=$(hook_cargo_package_for_path "$path" || true)
    [ -n "$package" ] && printf '%s\n' "$package" >> "$output"
  done < "$input"
  sort -u -o "$output" "$output"
}

# Format only the Cargo packages that own staged Rust files. Harn deliberately
# tracks malformed parser fixtures and generated/seed `.rs` files outside
# Cargo's format ownership, so invoking rustfmt directly on changed paths is
# not equivalent to `cargo fmt`. Return false for any unmapped Rust path so the
# caller preserves the workspace-wide Cargo fallback.
hook_write_rust_format_packages() {
  input=$1
  output=$2
  : > "$output"
  while IFS= read -r path; do
    case "$path" in
      *.rs|crates/*/Cargo.toml)
        package=$(hook_cargo_package_for_path "$path" || true)
        if [ -z "$package" ]; then
          : > "$output"
          return 1
        fi
        printf '%s\n' "$package" >> "$output"
        ;;
      Cargo.toml|Cargo.lock)
        : > "$output"
        return 1
        ;;
    esac
  done < "$input"
  sort -u -o "$output" "$output"
  [ -s "$output" ]
}

hook_run_rust_format_gate() {
  changed_file_list=$1
  changed_packages=$2

  if ! hook_paths_match "$changed_file_list" "$HOOK_RUST_PATTERN"; then
    echo "=== Pre-commit: skipping Rust formatting (no Rust/Cargo changes) ==="
    return
  fi

  if hook_write_rust_format_packages "$changed_file_list" "$changed_packages"; then
    set --
    while IFS= read -r package; do
      set -- "$@" -p "$package"
    done < "$changed_packages"
    echo "=== Pre-commit: checking Rust formatting for changed packages ($(tr '\n' ' ' < "$changed_packages")) ==="
    cargo fmt "$@" -- --check
  else
    echo "=== Pre-commit: checking workspace Rust formatting (unmapped Rust source) ==="
    cargo fmt --all -- --check
  fi
  echo "    Rust formatting OK."
}

# Run the one local Rust compilation gate for a commit lifecycle. Pre-commit
# owns formatting and cheap structural feedback; pre-push owns changed-package
# test-target compilation plus clippy so those two concerns share one Cargo
# profile/fingerprint instead of compiling overlapping graphs twice.
hook_run_rust_test_lint_gate() {
  changed_file_list=$1
  changed_packages=$2

  if hook_paths_match "$changed_file_list" '(^Cargo\.toml$|^Cargo\.lock$|^\.config/nextest\.toml$)'; then
    echo "=== Pre-push: running workspace Rust lint/test compile ==="
    cargo clippy --workspace --tests -- -D warnings
    return
  fi

  hook_write_changed_cargo_packages "$changed_file_list" "$changed_packages"
  if [ ! -s "$changed_packages" ]; then
    if hook_paths_match "$changed_file_list" "$HOOK_RUST_PATTERN"; then
      echo "=== Pre-push: running workspace Rust lint/test compile (no crate package matched) ==="
      cargo clippy --workspace --tests -- -D warnings
      return
    fi
    echo "=== Pre-push: skipping Rust lint/test compile (no changed crate packages) ==="
    return
  fi

  package_flags=""
  while IFS= read -r package; do
    package_flags="$package_flags -p $package"
  done < "$changed_packages"
  echo "=== Pre-push: running Rust lint/test compile for changed packages ($(tr '\n' ' ' < "$changed_packages")) ==="
  # shellcheck disable=SC2086  # intentional word splitting on -p flags
  cargo clippy $package_flags --tests -- -D warnings
}
