#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="${HARN_RELEASE_ROOT:-$(cd "$SCRIPT_DIR/.." && pwd)}"
cd "$ROOT_DIR"
PUBLISH_SCRIPT="${HARN_PUBLISH_SCRIPT:-./scripts/publish.sh}"
# shellcheck source=scripts/lib/cargo_env.sh
source "$SCRIPT_DIR/lib/cargo_env.sh"
# shellcheck source=scripts/lib/harn_bin.sh
source "$SCRIPT_DIR/lib/harn_bin.sh"

release_gate_target_name() {
  printf '%s' "$(basename "$ROOT_DIR")" | tr -c 'A-Za-z0-9._-' '-'
}

# The release gate's Cargo cache lives under the same durable storage root as
# every dev-setup profile, NOT under `$TMPDIR`. macOS prunes `/var/folders/.../T`
# by file age and removes individual files rather than whole trees, so a
# long-lived target there decays into intact directories and fingerprints with
# missing build-script outputs — Cargo then considers the script fresh and never
# regenerates them. That gets more likely the longer the cache survives, which
# is the opposite of what a cache should do, and it surfaces in the most
# expensive part of a release.
default_release_gate_target_dir() {
  local storage_root
  storage_root="${HARN_DEV_SETUP_STORAGE_ROOT:-${XDG_CACHE_HOME:-$HOME/.cache}/harn/dev-setup}"
  storage_root="${storage_root%/}"
  printf '%s/release-gate-target/%s\n' "$storage_root" "$(release_gate_target_name)"
}

default_release_gate_package_target_dir() {
  printf '%s-package-check\n' "${CARGO_TARGET_DIR%/}"
}

configure_release_gate_cargo_env() {
  if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
    export CARGO_TARGET_DIR
    CARGO_TARGET_DIR="$(default_release_gate_target_dir)"
  fi
  if [[ -z "${CARGO_BUILD_BUILD_DIR:-}" ]]; then
    harn_export_cargo_build_dir_for_target "$CARGO_TARGET_DIR" || true
  fi
  if [[ -z "${HARN_PACKAGE_VERIFY_TARGET_DIR:-}" ]]; then
    export HARN_PACKAGE_VERIFY_TARGET_DIR
    HARN_PACKAGE_VERIFY_TARGET_DIR="$(default_release_gate_package_target_dir)"
  fi
  if [[ -z "${HARN_PACKAGE_VERIFY_BUILD_DIR:-}" ]]; then
    export HARN_PACKAGE_VERIFY_BUILD_DIR
    HARN_PACKAGE_VERIFY_BUILD_DIR="$HARN_PACKAGE_VERIFY_TARGET_DIR/build"
  fi
}

release_gate_stale_out_dir_packages() {
  local diagnostics="$1"
  local output="$2"
  local build_dir="${3:-$CARGO_BUILD_BUILD_DIR}"
  local build_prefix="${build_dir%/}/debug/build/"
  local line remainder component package
  : > "$output"
  while IFS= read -r line; do
    [[ "$line" == *"No such file or directory"* ]] || continue
    remainder="${line#*"$build_prefix"}"
    [[ "$remainder" != "$line" ]] || continue
    component="${remainder%%/out/*}"
    [[ "$component" != "$remainder" ]] || continue
    if [[ ! "$component" =~ ^(.+)-[0-9a-f]{16}$ ]]; then
      echo "error: refused malformed stale build-script path component: $component" >&2
      : > "$output"
      return 2
    fi
    package="${BASH_REMATCH[1]}"
    if [[ ! "$package" =~ ^[A-Za-z0-9_][A-Za-z0-9_.-]*$ ]]; then
      echo "error: refused malformed Cargo package name from stale build-script path: $package" >&2
      : > "$output"
      return 2
    fi
    printf '%s\n' "$package" >> "$output"
  done < "$diagnostics"
  sort -u -o "$output" "$output"
  [[ -s "$output" ]]
}

release_gate_clean_stale_out_dir_packages() {
  local operation="$1"
  local packages="$2"
  local target_dir="${3:-$CARGO_TARGET_DIR}"
  local build_dir="${4:-$CARGO_BUILD_BUILD_DIR}"
  local -a clean_args=(clean)
  local package
  while IFS= read -r package; do
    clean_args+=(-p "$package")
  done < "$packages"

  local recovery_started recovery_elapsed
  recovery_started="$(date +%s)"
  printf 'recovery: stale Cargo build-script outputs detected for %s (packages=%s)\n' \
    "$operation" "$(paste -sd, "$packages")"
  if ! CARGO_TARGET_DIR="$target_dir" CARGO_BUILD_BUILD_DIR="$build_dir" \
    cargo "${clean_args[@]}"; then
    echo "error: package-scoped stale build-script cleanup failed for $operation" >&2
    return 1
  fi
  recovery_elapsed=$(( $(date +%s) - recovery_started ))
  printf 'recovery: package-scoped Cargo cleanup complete for %s (%ss)\n' \
    "$operation" "$recovery_elapsed"
}

# How many recovery rounds one operation may spend. Cargo reports only the
# FIRST missing build-script output it trips over, so a single classification
# can never name more than the packages that failed on that attempt. A decayed
# cache holds several, and cleaning one at a time used to exhaust a retry
# budget of one: a v0.10.55 attempt cleaned `libsqlite3-sys` for `package-audit`
# and then went terminal on `tree-sitter`, a package the classifier had already
# named correctly for a sibling lane moments earlier.
RELEASE_GATE_STALE_RECOVERY_ROUNDS="${RELEASE_GATE_STALE_RECOVERY_ROUNDS:-4}"

# Discard a target directory whose decay has outrun per-package classification.
# Only ever called with a directory this script derived or a caller explicitly
# configured, and only after a round classified nothing it had not already
# cleaned.
release_gate_clear_stale_target_dir() {
  local operation="$1"
  local target_dir="$2"
  local build_dir="$3"
  if [[ -z "$target_dir" || "$target_dir" != /* || "$target_dir" == "/" ]]; then
    echo "error: refused to clear implausible target dir for $operation: '$target_dir'" >&2
    return 1
  fi
  printf 'recovery: package-scoped cleanup found nothing new for %s; clearing %s\n' \
    "$operation" "$target_dir"
  rm -rf -- "$target_dir"
  if [[ -n "$build_dir" && "$build_dir" == /* && "$build_dir" != "/" \
    && "$build_dir" != "$target_dir"/* && "$build_dir" != "$target_dir" ]]; then
    rm -rf -- "$build_dir"
  fi
}

# Spend one recovery round on `diagnostics`, cleaning whatever it names that
# `cleaned` has not already accounted for. `cleaned` and `fallback_state` carry
# state across rounds for a single operation; the caller owns both files.
#
# Exit status:
#   0  cleaned a package set no earlier round had seen — a retry is warranted
#   1  the failure names no stale build-script output; not recoverable
#   2  classification failed closed
#   3  nothing new was named, so the whole target directory was cleared instead
#   4  the target directory had already been cleared; out of moves
#   5  the cleanup itself failed
release_gate_recover_stale_out_dir_round() {
  local operation="$1"
  local diagnostics="$2"
  local cleaned="$3"
  local fallback_state="$4"
  local target_dir="${5:-$CARGO_TARGET_DIR}"
  local build_dir="${6:-$CARGO_BUILD_BUILD_DIR}"

  local packages fresh classification_status=0
  packages="$(mktemp)"
  fresh="$(mktemp)"
  release_gate_stale_out_dir_packages "$diagnostics" "$packages" "$build_dir" \
    || classification_status=$?
  if [[ "$classification_status" -ne 0 ]]; then
    rm -f "$packages" "$fresh"
    return "$classification_status"
  fi

  comm -23 "$packages" "$cleaned" > "$fresh"
  if [[ -s "$fresh" ]]; then
    if ! release_gate_clean_stale_out_dir_packages \
      "$operation" "$fresh" "$target_dir" "$build_dir"; then
      rm -f "$packages" "$fresh"
      return 5
    fi
    cat "$fresh" >> "$cleaned"
    sort -u -o "$cleaned" "$cleaned"
    rm -f "$packages" "$fresh"
    return 0
  fi
  rm -f "$packages" "$fresh"

  if [[ -f "$fallback_state" ]]; then
    return 4
  fi
  : > "$fallback_state"
  release_gate_clear_stale_target_dir "$operation" "$target_dir" "$build_dir" || return 5
  return 3
}

# Report a terminal recovery status. Shared so both recovery sites describe the
# same outcome the same way.
release_gate_report_stale_recovery_failure() {
  local operation="$1"
  local status="$2"
  case "$status" in
    1) echo "error: $operation failed without a recoverable stale build-script output" >&2 ;;
    2) echo "error: $operation stale-output classification failed closed" >&2 ;;
    4) echo "error: $operation still failed after its target directory was cleared" >&2 ;;
    5) echo "error: $operation stale build-script cleanup failed" >&2 ;;
    *) echo "error: $operation exhausted its stale build-script recovery rounds" >&2 ;;
  esac
}

release_gate_run_with_stale_out_dir_recovery() {
  local operation="$1"
  shift
  local diagnostics cleaned fallback_state
  diagnostics="$(mktemp)"
  cleaned="$(mktemp)"
  # Existence is the flag, so reserve the name and remove the file.
  fallback_state="$(mktemp)"
  rm -f "$fallback_state"
  local round=0 recovery_status=0

  while :; do
    if "$@" 2> "$diagnostics"; then
      rm -f "$diagnostics" "$cleaned" "$fallback_state"
      if [[ "$round" -gt 0 ]]; then
        echo "recovery: $operation succeeded after stale build-script cleanup"
      fi
      return 0
    fi
    cat "$diagnostics" >&2

    if [[ "$round" -ge "$RELEASE_GATE_STALE_RECOVERY_ROUNDS" ]]; then
      rm -f "$diagnostics" "$cleaned" "$fallback_state"
      release_gate_report_stale_recovery_failure "$operation" 0
      return 1
    fi

    recovery_status=0
    release_gate_recover_stale_out_dir_round \
      "$operation" "$diagnostics" "$cleaned" "$fallback_state" || recovery_status=$?
    if [[ "$recovery_status" -ne 0 && "$recovery_status" -ne 3 ]]; then
      rm -f "$diagnostics" "$cleaned" "$fallback_state"
      release_gate_report_stale_recovery_failure "$operation" "$recovery_status"
      return 1
    fi

    round=$(( round + 1 ))
    printf 'recovery: retrying %s (round %s of %s)\n' \
      "$operation" "$round" "$RELEASE_GATE_STALE_RECOVERY_ROUNDS"
  done
}

release_gate_warm_prebuild() {
  release_gate_run_with_stale_out_dir_recovery \
    "warm prebuild" cargo build -p harn-cli --bin harn --quiet
}

release_gate_prepare_cli_aot() {
  release_gate_run_with_stale_out_dir_recovery \
    "shared CLI AOT preparation" make gen-cli-aot
}

RELEASE_PREPARE_AOT_BIN=""

release_gate_snapshot_prepare_aot_generator() {
  local destination_dir="$1"
  local source_bin="${HARN_RELEASE_CLI_AOT_GEN_BIN:-}"
  if [[ -n "$source_bin" ]]; then
    harn_require_executable_bin "$source_bin" || return $?
  else
    release_gate_run_with_stale_out_dir_recovery \
      "release AOT generator prebuild" \
      cargo build -p harn-cli-aot-gen --bin harn-cli-aot-gen --quiet
    source_bin="$(harn_debug_named_binary_path harn-cli-aot-gen)"
  fi
  RELEASE_PREPARE_AOT_BIN="$(
    harn_snapshot_binary "$source_bin" "$destination_dir" harn-cli-aot-gen
  )"
}

# One immutable CLI for every prepare-time Harn tool, snapshotted before the
# workspace version is rewritten.
#
# `prepare` runs several `.harn` metadata and projection tools while mutating
# the workspace version and its generated metadata. Each mutation invalidates
# Cargo fingerprints, so resolving the CLI through `cargo run` per tool
# recompiled the runtime graph repeatedly inside one nominal shell step — the
# top-level transcript showed a single multi-minute `prepare` and hid it.
#
# The semantics these tools need are the exact PRE-mutation candidate
# semantics, so one binary built once is not a compromise; it is the correct
# input. The candidate under test is still rebuilt from the post-mutation tree
# and audited separately.
#
# Ambient `HARN_BIN` is deliberately NOT consulted: it may be a stale developer
# build, and prepare has no way to prove its source identity. Only an explicit
# `HARN_RELEASE_TOOLS_BIN` — a caller asserting exactly that — is honored.
RELEASE_PREPARE_TOOLS_BIN=""
# Set by `cmd_audit` to the binary this gate built and audited from this tree.
RELEASE_GATE_AUDITED_HARN_BIN=""

release_gate_snapshot_prepare_tools_cli() {
  local destination_dir="$1"
  local source_bin="${HARN_RELEASE_TOOLS_BIN:-}"
  if [[ -n "$source_bin" ]]; then
    harn_require_executable_bin "$source_bin" || return $?
  elif [[ -n "$RELEASE_GATE_AUDITED_HARN_BIN" ]]; then
    # `full` already built, snapshotted, and audited this exact tree. Reusing
    # that binary is both cheaper than a second build and better provenance
    # than one, so use it in place rather than copying it again.
    harn_require_executable_bin "$RELEASE_GATE_AUDITED_HARN_BIN" || return $?
    RELEASE_PREPARE_TOOLS_BIN="$RELEASE_GATE_AUDITED_HARN_BIN"
    printf 'ok: %-15s (%s)\n' "release-tools" "$RELEASE_PREPARE_TOOLS_BIN"
    return 0
  else
    release_gate_run_with_stale_out_dir_recovery \
      "release tools CLI prebuild" \
      cargo build -p harn-cli --bin harn --quiet
    source_bin="$(harn_debug_named_binary_path harn)"
  fi
  RELEASE_PREPARE_TOOLS_BIN="$(
    harn_snapshot_binary "$source_bin" "$destination_dir" harn-release-tools
  )"
  printf 'ok: %-15s (%s)\n' "release-tools" "$RELEASE_PREPARE_TOOLS_BIN"
}

usage() {
  cat <<'EOF'
Usage:
  ./scripts/release_gate.sh audit [--receipt path] [--source-only] [--validate-only]
  ./scripts/release_gate.sh prepare --bump patch
  ./scripts/release_gate.sh publish [--dry-run]
  ./scripts/release_gate.sh notes [--version vX.Y.Z] [--output file]
  ./scripts/release_gate.sh full --bump patch [--dry-run]

Commands:
  audit    Run the full audit, source-only lanes, or receipt-authorized residual lanes.
  prepare  Bump the workspace version locally and print next tag/release steps.
  publish  Publish crates with scripts/publish.sh and print tag/release follow-up.
  notes    Render GitHub release notes for a version from CHANGELOG.md.
  full     Run audit, prepare, and publish in sequence.
EOF
}

configure_release_gate_cargo_env

require_clean_tree() {
  if ! git diff --quiet --ignore-submodules HEAD --; then
    echo "error: working tree is dirty"
    echo "hint: commit or stash changes before prepare/publish"
    exit 1
  fi
}

current_version() {
  release_metadata current
}

release_metadata() {
  if [[ -n "${HARN_RELEASE_METADATA_BIN:-}" ]]; then
    "$HARN_RELEASE_METADATA_BIN" run "$SCRIPT_DIR/release_metadata.harn" -- "$@" --root "$ROOT_DIR"
  else
    harn_cmd run "$SCRIPT_DIR/release_metadata.harn" -- "$@" --root "$ROOT_DIR"
  fi
}

next_version() {
  local bump="$1"
  local preid="${2:-}"
  if [[ "$bump" != "patch" || -n "$preid" ]]; then
    echo "error: stable releases strip the declared X.Y.Z-dev target; use --bump patch without --preid" >&2
    return 1
  fi
  release_metadata release-target
}

bump_version() {
  local next="$1"
  local bump="$2"
  local preid="${3:-}"
  if [[ -n "$preid" ]]; then
    release_metadata apply --version "$next" --bump "$bump" --preid "$preid"
  else
    release_metadata apply --version "$next" --bump "$bump"
  fi
}

reconcile_cargo_lock() {
  # Cargo owns Cargo.lock's format and package graph. Let it make the minimal
  # reconciliation required by the release metadata rewrite, then prove a
  # second locked resolution is a no-op before any release content is staged.
  cargo metadata --format-version=1 >/dev/null
  cargo metadata --format-version=1 --locked >/dev/null
}

# Wrap a command with a banner + duration. Used by the per-audit
# substep helpers so the parallel audit log shows which sub-phase in
# `rust-audit` / `harn-audit` / etc is the long pole.
time_phase() {
  local label="$1"
  shift
  local started
  started="$(date +%s)"
  printf '  -> %s ...\n' "$label"
  "$@"
  local rc=$?
  printf '  <- %s (%ss)\n' "$label" "$(( $(date +%s) - started ))"
  return "$rc"
}

harn_cmd() {
  # Prepare snapshots one exact-source CLI before mutating the workspace and
  # routes every Harn tool through it, so a metadata rewrite cannot force
  # another compilation of the shared runtime graph mid-step.
  if [[ -n "${RELEASE_PREPARE_TOOLS_BIN:-}" ]]; then
    "$RELEASE_PREPARE_TOOLS_BIN" "$@"
  elif [[ -n "${HARN_BIN:-}" ]]; then
    "$HARN_BIN" "$@"
  else
    cargo run --quiet --bin harn -- "$@"
  fi
}

file_sha256() {
  local path="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$path" | awk '{print $1}'
    return 0
  fi
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$path" | awk '{print $1}'
    return 0
  fi
  echo "error: sha256sum or shasum is required to validate the warmed Harn binary" >&2
  return 1
}

run_docs_audit() {
  if ! command -v npm >/dev/null 2>&1; then
    echo "error: npm (Node.js) is required for the release docs audit" >&2
    return 1
  fi
  time_phase "markdownlint" npx markdownlint-cli2 "**/*.md"
  time_phase "docs site build" ./scripts/build_docs_site.sh
  time_phase "documentation contracts" make -j4 check-docs
}

run_generated_audit() {
  time_phase "language-spec drift" make check-language-spec
  time_phase "highlight drift" make check-highlight
  time_phase "CLI AOT drift" make check-cli-aot
  time_phase "protocol artifact drift" \
    make check-protocol-artifacts PROTOCOL_ARTIFACT_VERSION="$(current_version)"
  time_phase "connector schema drift" make check-connector-schemas
  time_phase "harness migration table drift" make check-harness-migrations
  time_phase "session bundle schema drift" make check-session-bundle-schema
  time_phase "run-view fixture drift" make check-run-view-fixtures
}

run_grammar_audit() {
  # Grammar-parse regressions are a common, cheap-to-detect defect class, so run
  # the tree-sitter parse sweep FIRST in this lane. It depends only on the grammar
  # deps (npm ci) and the already-warm CLI, not on the spec-verification phases
  # below, so hoisting it makes a parse failure surface in seconds after npm ci
  # instead of after the metadata/spec checks.
  if [[ ! -d tree-sitter-harn ]]; then
    echo "error: tree-sitter-harn is required for the release grammar audit" >&2
    return 1
  fi
  if ! command -v npm >/dev/null 2>&1; then
    echo "error: npm (Node.js) is required for the release grammar audit" >&2
    return 1
  fi
  time_phase "tree-sitter npm ci" "$SCRIPT_DIR/npm_ci_with_retry.sh" tree-sitter-harn
  time_phase "verify_tree_sitter_parse" harn_cmd run scripts/verify_tree_sitter_parse.harn -- --strict
  time_phase "tree-sitter npm test" bash -c "cd tree-sitter-harn && npm test"
  if [[ ! -f spec/HARN_SPEC.md ]]; then
    echo "error: missing spec/HARN_SPEC.md"
    return 1
  fi
  time_phase "verify_release_metadata" harn_cmd run scripts/verify_release_metadata.harn
  # NOTE: `sync_language_spec` is intentionally NOT run here. It is the docs
  # mirror writer (spec/HARN_SPEC.md -> docs/src/language-spec.md) and already
  # runs in `run_docs_audit`, which executes in a sibling parallel lane. Running
  # it in both lanes both duplicated ~72s of work and raced two writers on the
  # same `docs/src/language-spec.md` output. `verify_language_spec` below reads
  # the canonical spec source directly (SPEC_PATH = spec/HARN_SPEC.md), not the
  # mirror, so it does not depend on the sync having run in this lane.
  time_phase "verify_language_spec" harn_cmd run scripts/verify_language_spec.harn
}

run_security_audit() {
  echo "=== Security/trust boundary audit ==="
  time_phase "boundary-keyword grep" \
    rg -n "OAuth|oauth|MCP|trust boundary|mutation session|worker_update|tool/pre_use|tool/post_use" \
      README.md docs/src crates/harn-vm crates/harn-cli .github CLAUDE.md >/dev/null
}

release_rust_test() {
  # Blacksmith's Ubuntu image does not expose Landlock. Harn CI therefore runs
  # every environment-neutral workspace test there and owns the six
  # OS-confinement assertions on GitHub Ubuntu. Release candidates are
  # generated metadata-only commits over a merge-queue-proven parent, so the
  # hosted release audit must preserve that same partition instead of turning
  # a missing host kernel feature into a product failure.
  local host_bound_filter='test(test_linux_process_sandbox_catches_ten_process_escapes) or test(workspace_env_integration) or test(local_backend_execs_inside_session_outputs) or test(local_backend_timeout_is_enforced_without_shell_timeout_binary) or test(sandboxed_npm_install_resolves_file_tarball_dependency_offline)'
  if [[ "${HARN_RUNNER_TIER:-}" == "blacksmith" ]]; then
    echo "release rust audit: Blacksmith tier; Landlock-only tests remain owned by GitHub Ubuntu CI"
    make test ARGS="--workspace -E 'not (${host_bound_filter})'"
  else
    make test
  fi
}

run_rust_audit() {
  time_phase "cargo fmt --check" make fmt-check
  time_phase "prompt text ownership" make lint-no-rust-prompt-prose
  time_phase "cargo clippy --workspace --all-targets" \
    ./scripts/ci/run_rust_lint_lane.sh
  time_phase "make test (cargo-nextest)" release_rust_test
}

run_harn_audit() {
  time_phase "harn conformance" make conformance
  time_phase "protocol conformance" make protocol-conformance
  time_phase "harn lint" make lint-harn
  time_phase "harn fmt --check" make fmt-harn
}

run_harn_performance_audit() {
  time_phase "parallel test-case performance" make check-test-case-performance
}

# Host-platform reproduction of the cross-platform release smoke
# matrix. CI exercises the full macOS/Linux/Windows fan-out via
# .github/workflows/release-smoke.yml; this lane catches host-side
# regressions (binary failing to start, generated artifact emitter
# drift, mock provider plumbing) before a maintainer pushes a release
# tag. Uses the debug binary populated by the warm prebuild so the
# lane does not fight the cargo lock with rust-audit's clippy +
# nextest. The cross-platform deltas still depend on the CI matrix.
run_smoke_audit() {
  time_phase "release smoke" make smoke-audit
}

run_package_audit() {
  ./scripts/verify_crate_packages.sh
}

SELECTED_AUDIT_STEPS=()
SELECTED_AUDIT_RUNNERS=()
AUDIT_PLAN_REASON=""
AUDIT_RECEIPT_REUSED="false"
PRESERVE_AUDIT_TMP=0
AUDIT_TMP_DIR=""

# Validate lane-owned tools before the warm build or shared AOT preparation.
# A missing audit dependency is an environment error, not evidence about the
# candidate, and discovering it after those expensive phases wastes the whole
# attempt. Keep this keyed by semantic lane so receipt/source-only plans only
# require the tools they will actually execute.
release_gate_preflight_audit_tools() {
  local -a required=(cargo git make)
  local step tool
  for step in "${SELECTED_AUDIT_STEPS[@]}"; do
    case "$step" in
      rust-audit)
        required+=(cargo-nextest)
        ;;
      docs-audit | grammar-audit)
        required+=(npm)
        ;;
      security-audit)
        required+=(rg)
        ;;
    esac
  done

  local -a missing=()
  local seen=" "
  for tool in "${required[@]}"; do
    [[ "$seen" == *" $tool "* ]] && continue
    seen+="$tool "
    command -v "$tool" >/dev/null 2>&1 || missing+=("$tool")
  done
  if [[ "${#missing[@]}" -gt 0 ]]; then
    printf 'error: release audit prerequisites missing: %s\n' "${missing[*]}" >&2
    if [[ " ${missing[*]} " == *" cargo-nextest "* ]]; then
      echo "hint: run 'make setup' or install cargo-nextest before retrying" >&2
    fi
    return 1
  fi
}

# Render a lane's terminal exit status. A lane killed by a signal writes
# nothing recognizable into its log, so the status is the only record of how it
# died; name it rather than leaving an unexplained failing lane.
release_gate_lane_exit_description() {
  local rc_file="$1"
  local rc=""
  local signal
  local name
  if [[ -f "$rc_file" ]]; then
    rc="$(<"$rc_file")"
  fi
  if [[ ! "$rc" =~ ^[0-9]+$ ]]; then
    printf 'exit status unknown\n'
    return 0
  fi
  if [[ "$rc" -gt 128 ]]; then
    signal=$(( rc - 128 ))
    name="$(kill -l "$signal" 2>/dev/null || true)"
    if [[ -n "$name" ]]; then
      printf 'killed by SIG%s (exit %s)\n' "$name" "$rc"
    else
      printf 'killed by signal %s (exit %s)\n' "$signal" "$rc"
    fi
    return 0
  fi
  printf 'exit %s\n' "$rc"
}

cleanup_preserved_audit_tmp() {
  if [[ -n "$AUDIT_TMP_DIR" ]]; then
    rm -rf "$AUDIT_TMP_DIR"
    AUDIT_TMP_DIR=""
  fi
}

resolve_audit_plan() {
  local receipt_path="$1"
  local plan_path="$2"
  local certified_source_sha="$3"
  local source_only="$4"
  local args=(
    run scripts/release_audit_contract.harn --
    --contract scripts/release_audit_contract.json
    --check-ci .github/workflows/ci.yml
    --head-sha "$certified_source_sha"
    --shell-plan
  )
  if [[ -n "$receipt_path" ]]; then
    args+=(--receipt "$receipt_path")
    local warm_binary_sha256=""
    if [[ -n "${HARN_BIN:-}" && -x "$HARN_BIN" ]]; then
      warm_binary_sha256="$(file_sha256 "$HARN_BIN")"
    fi
    args+=(--warm-binary-sha256 "$warm_binary_sha256")
  elif [[ "$source_only" -eq 1 ]]; then
    args+=(--source-only)
  fi
  harn_cmd "${args[@]}" > "$plan_path"

  SELECTED_AUDIT_STEPS=()
  SELECTED_AUDIT_RUNNERS=()
  local kind first second
  local meta_seen=0
  while IFS=$'\t' read -r kind first second; do
    case "$kind" in
      meta)
        if [[ "$meta_seen" -ne 0 || ! "$first" =~ ^(true|false)$ || ! "$second" =~ ^[a-z0-9_]+$ ]]; then
          echo "error: invalid release audit plan metadata" >&2
          return 1
        fi
        AUDIT_RECEIPT_REUSED="$first"
        AUDIT_PLAN_REASON="$second"
        meta_seen=1
        ;;
      lane)
        if [[ ! "$first" =~ ^[a-z0-9-]+$ || ! "$second" =~ ^[a-z0-9_]+$ ]]; then
          echo "error: invalid release audit lane metadata" >&2
          return 1
        fi
        if ! declare -F "$second" >/dev/null; then
          echo "error: missing audit lane runner for $first: $second" >&2
          return 1
        fi
        SELECTED_AUDIT_STEPS+=("$first")
        SELECTED_AUDIT_RUNNERS+=("$second")
        ;;
      *)
        echo "error: invalid release audit plan row" >&2
        return 1
        ;;
    esac
  done < "$plan_path"
  if [[ "$meta_seen" -ne 1 || "${#SELECTED_AUDIT_STEPS[@]}" -eq 0 ]]; then
    echo "error: incomplete release audit plan" >&2
    return 1
  fi
  if [[ -n "$receipt_path" && "$AUDIT_RECEIPT_REUSED" != "true" ]]; then
    echo "error: hosted audit receipt rejected: $AUDIT_PLAN_REASON" >&2
    return 1
  fi
}

cmd_audit() {
  local receipt_path=""
  local source_only=0
  local validate_only=0
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --receipt)
        if [[ $# -lt 2 || -z "${2:-}" ]]; then
          echo "error: audit --receipt requires a path" >&2
          exit 1
        fi
        receipt_path="$2"
        shift 2
        ;;
      --validate-only)
        validate_only=1
        shift
        ;;
      --source-only)
        source_only=1
        shift
        ;;
      *)
        echo "error: unknown audit arg: $1" >&2
        usage
        exit 1
        ;;
    esac
  done
  if [[ "$source_only" -eq 1 && -n "$receipt_path" ]]; then
    echo "error: audit --source-only cannot be combined with --receipt" >&2
    exit 1
  fi

  local plan_path
  plan_path="$(mktemp)"
  local certified_source_sha
  certified_source_sha="$(git rev-parse HEAD)"
  if ! resolve_audit_plan "$receipt_path" "$plan_path" "$certified_source_sha" "$source_only"; then
    rm -f "$plan_path"
    exit 1
  fi
  rm -f "$plan_path"

  printf 'audit plan: %s (receipt_reused=%s, lanes=%s)\n' \
    "$AUDIT_PLAN_REASON" "$AUDIT_RECEIPT_REUSED" "${SELECTED_AUDIT_STEPS[*]}"
  if [[ "$validate_only" -eq 1 ]]; then
    return 0
  fi
  release_gate_preflight_audit_tools || exit 1

  echo "=== Parallel release audit ==="
  export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"
  local audit_started
  audit_started="$(date +%s)"

  # Serial warm prebuild before spawning the parallel lanes. The Harn/script
  # lanes only need a runnable CLI; rust-audit still owns the full clippy +
  # nextest coverage, and package-audit still owns extracted crate checks. Build
  # the CLI binary once up front so those lanes do not race rust-audit for a
  # plain Harn invocation, without front-loading a duplicate workspace build.
  local prebuild_started prebuild_elapsed
  prebuild_started="$(date +%s)"
  local cargo_harn_bin=""
  if [[ ( "$AUDIT_RECEIPT_REUSED" == "true" || "$source_only" -eq 1 ) && -n "${HARN_BIN:-}" && -x "$HARN_BIN" ]]; then
    cargo_harn_bin="$HARN_BIN"
    echo ">>> warm-prebuild (reuse exact receipt-warmed HARN_BIN)"
  else
    echo ">>> warm-prebuild (cargo build -p harn-cli --bin harn)"
    if ! release_gate_warm_prebuild; then
      exit 1
    fi
    cargo_harn_bin="$(HARN_BIN='' HARN_BIN_NO_BUILD=0 "$SCRIPT_DIR/harn_bin.sh" --print)"
  fi
  prebuild_elapsed=$(( $(date +%s) - prebuild_started ))
  printf 'ok: %-15s (%ss)\n' "warm-prebuild" "$prebuild_elapsed"
  if [[ ! -x "$cargo_harn_bin" ]]; then
    echo "error: warm prebuild completed but HARN_BIN is not executable: $cargo_harn_bin"
    exit 1
  fi

  local tmp
  tmp="$(mktemp -d)"
  if [[ "$PRESERVE_AUDIT_TMP" -eq 1 ]]; then
    AUDIT_TMP_DIR="$tmp"
  fi
  local stable_bin_dir stable_harn_bin
  stable_bin_dir="$tmp/harn-bin"
  stable_harn_bin="$(harn_snapshot_binary "$cargo_harn_bin" "$stable_bin_dir")"
  HARN_BIN="$stable_harn_bin"
  HARN_CONFORMANCE_HARN_BIN="$stable_harn_bin"
  export HARN_BIN HARN_CONFORMANCE_HARN_BIN
  # This gate built and audited this exact binary from this exact tree, so a
  # later `prepare` in the same run may use it for its Harn tools. Recorded
  # under its own name rather than read back from `HARN_BIN`, which an ambient
  # environment can also set and whose source identity prepare cannot check.
  RELEASE_GATE_AUDITED_HARN_BIN="$stable_harn_bin"
  printf 'ok: %-15s (%s)\n' "harn-bin" "$HARN_BIN"

  # crates/harn-cli/generated/ is gitignored build input shared by rust-audit
  # and package-audit. Generate it once before either parallel consumer starts:
  # generating inside rust-audit lets package-audit copy a mixed old/new
  # payload while the writer replaces the manifest and bytecode files.
  #
  # The residual receipt plan omits rust-audit and verifies the already-bumped
  # payload with check-cli-aot, so keep this preparation scoped to plans that
  # actually run the source Rust lane.
  local prepare_cli_aot=0
  local selected_step
  for selected_step in "${SELECTED_AUDIT_STEPS[@]}"; do
    if [[ "$selected_step" == "rust-audit" ]]; then
      prepare_cli_aot=1
      break
    fi
  done
  if [[ "$prepare_cli_aot" -eq 1 ]]; then
    time_phase "prepare shared CLI AOT payload" release_gate_prepare_cli_aot
  fi

  echo "audit lane log dir: $tmp"
  local -a steps=()
  local -a pids=()
  local -a runners=()
  local -a failed_lane_indices=()
  local needs_harn_performance=0

  # Lane concurrency is a property of the machine, not of the audit. Each heavy
  # lane owns an internal pool (Cargo in `rust-audit` and `package-audit`, Harn
  # workers in `harn-audit`). Leaving every pool at the host default multiplies
  # the advertised CPU count by the number of live lanes, so startup contention
  # can exceed a nested process timeout that exists as a hang backstop rather
  # than a performance budget.
  #
  # `ci.yml` gives conformance its own runner. This gate has one machine: small
  # hosts serialize heavy lanes, while wider hosts partition their worker
  # budget at this scheduler boundary and retain useful overlap.
  local lane_cpus="${HARN_RELEASE_GATE_LANE_CPUS:-}"
  if [[ -z "$lane_cpus" ]]; then
    lane_cpus="$(getconf _NPROCESSORS_ONLN 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 0)"
  fi
  # Two cores per lane is the smallest split under which neither pool is
  # reduced to a single worker. Below that, overlap costs more than it buys.
  local serial_lanes=0
  local serialize_heavy_lanes=0
  local heavy_lane_count=0
  local heavy_lane_worker_budget=0
  local selected_lane
  for selected_lane in "${SELECTED_AUDIT_STEPS[@]}"; do
    case "$selected_lane" in
      rust-audit | harn-audit | package-audit)
        heavy_lane_count=$((heavy_lane_count + 1))
        ;;
    esac
  done
  if [[ "$lane_cpus" -gt 0 && "$lane_cpus" -lt 4 ]]; then
    serial_lanes=1
    printf 'audit lanes: serial (%s cpu for %s lanes)\n' \
      "$lane_cpus" "${#SELECTED_AUDIT_STEPS[@]}"
  elif [[ "$lane_cpus" -gt 0 && "$lane_cpus" -lt $(( ${#SELECTED_AUDIT_STEPS[@]} * 2 )) ]]; then
    # Rust, Harn conformance, and package verification each own an internal
    # worker/compiler pool. Serialize only those resource-heavy lanes while
    # allowing single-process docs/generated/grammar/security/smoke work to
    # overlap. This avoids both hosted-runner starvation and the old eight-lane
    # serial tail on ordinary 12-core build servers.
    serialize_heavy_lanes=1
    printf 'audit lanes: resource-aware (%s cpu; heavy lanes serialized, light lanes parallel)\n' \
      "$lane_cpus"
  elif [[ "$lane_cpus" -gt 0 && "$heavy_lane_count" -gt 1 ]]; then
    # A wide host still needs a budget: Cargo otherwise gives *each* concurrent
    # rust/package lane the full host worker count while conformance owns its
    # own process pool. Partition the host once at this scheduler boundary so
    # the configured pools cannot oversubscribe it. Explicit lane env remains
    # an operator-owned override for diagnosis.
    heavy_lane_worker_budget=$((lane_cpus / heavy_lane_count))
    [[ "$heavy_lane_worker_budget" -lt 1 ]] && heavy_lane_worker_budget=1
    printf 'audit lanes: bounded parallel (%s cpu; %s heavy lanes; %s workers per heavy lane)\n' \
      "$lane_cpus" "$heavy_lane_count" "$heavy_lane_worker_budget"
  fi

  # Each step writes its wall-clock duration to `<name>.dur` so the
  # parent can report per-step timings once everyone wraps. That lets
  # the release gate call out which audit lane is the long pole.
  # With the warm prebuild above, lanes should complete in parallel
  # without fighting for the cargo lock; any lane blowing past ~5 min
  # is a real regression worth investigating.
  run_step() {
    local name="$1"
    shift
    local started
    started="$(date +%s)"
    # Suspend errexit around the lane so a failure reaches the duration and
    # status writes below instead of aborting this function. `|| rc=$?` and
    # `if` both work by putting the lane in a conditional context, which
    # suppresses errexit inside the subshell too and lets a lane keep running
    # past its own first failing sub-step.
    local rc=0
    set +e
    (
      set -euo pipefail
      if [[ "$heavy_lane_worker_budget" -gt 0 ]]; then
        case "$name" in
          rust-audit | package-audit)
            export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-$heavy_lane_worker_budget}"
            ;;
          harn-audit)
            local conformance_lane_jobs="$heavy_lane_worker_budget"
            [[ "$conformance_lane_jobs" -gt 4 ]] && conformance_lane_jobs=4
            export HARN_CONFORMANCE_JOBS="${HARN_CONFORMANCE_JOBS:-$conformance_lane_jobs}"
            ;;
        esac
      fi
      echo ">>> $name"
      "$@"
    ) >"$tmp/$name.log" 2>&1
    rc=$?
    set -e
    printf '%s\n' "$(( $(date +%s) - started ))" >"$tmp/$name.dur"
    # A lane killed by a signal writes nothing recognizable to its log, so the
    # exit status is the only record of how it died. Keep it for the summary.
    printf '%s\n' "$rc" >"$tmp/$name.rc"
    return "$rc"
  }

  local last_heavy_lane_idx=-1
  launch_step() {
    local name="$1"
    shift
    local is_heavy=0
    case "$name" in
      rust-audit | harn-audit | package-audit) is_heavy=1 ;;
    esac
    if [[ "$serialize_heavy_lanes" -eq 1 && "$is_heavy" -eq 1 \
      && "$last_heavy_lane_idx" -ge 0 ]]; then
      local prior_pid="${pids[$last_heavy_lane_idx]}"
      if [[ -n "$prior_pid" ]]; then
        wait "$prior_pid" || true
        pids[$last_heavy_lane_idx]=""
      fi
    fi
    printf 'log: %-15s (%s)\n' "$name" "$tmp/$name.log"
    run_step "$name" "$@" &
    local launched="$!"
    steps+=("$name")
    runners+=("$1")
    if [[ "$serial_lanes" -eq 1 ]]; then
      # Settle this lane before the next one launches. The status is recorded
      # in `<name>.rc` by `run_step`, so the settle loop below reads that
      # instead of waiting on a pid it has already reaped. Record an empty pid
      # to mark the lane as already settled.
      wait "$launched" || true
      pids+=("")
      return
    fi
    pids+=("$launched")
    if [[ "$serialize_heavy_lanes" -eq 1 && "$is_heavy" -eq 1 ]]; then
      last_heavy_lane_idx=$(( ${#pids[@]} - 1 ))
    fi
  }

  local lane_idx
  for lane_idx in "${!SELECTED_AUDIT_STEPS[@]}"; do
    # Exact-candidate source certification (`--source-only`) runs alongside
    # macos-nightly.yml at the same immutable SHA. That hosted job owns the
    # wall-clock performance ratchet; running it again on a shared developer
    # workstation measures unrelated worktree contention and can discard an
    # otherwise-green release after every functional lane has completed.
    # Full local audits retain the benchmark for direct diagnosis.
    if [[ "$source_only" -eq 0 && "${SELECTED_AUDIT_STEPS[$lane_idx]}" == "harn-audit" ]]; then
      needs_harn_performance=1
    fi
    launch_step "${SELECTED_AUDIT_STEPS[$lane_idx]}" "${SELECTED_AUDIT_RUNNERS[$lane_idx]}"
  done

  local failed=0
  # Names of lanes that are still failing once recovery settles. The failure
  # summary is driven by this list rather than by scanning logs for error text,
  # so a lane that dies without writing one is still reported.
  local failed_steps=()
  local idx
  for idx in "${!steps[@]}"; do
    local step="${steps[$idx]}"
    local pid="${pids[$idx]}"
    local dur=""
    # A serialized lane was already reaped in `launch_step`, so its exit status
    # survives only in `<step>.rc`. A missing file means the lane died without
    # writing one, which counts as a failure.
    local lane_rc=0
    if [[ -n "$pid" ]]; then
      wait "$pid" || lane_rc=$?
    elif [[ -f "$tmp/$step.rc" ]]; then
      lane_rc="$(cat "$tmp/$step.rc")"
    else
      lane_rc=1
    fi
    if [[ "$lane_rc" -eq 0 ]]; then
      dur="$([[ -f "$tmp/$step.dur" ]] && cat "$tmp/$step.dur" || echo '?')"
      printf 'ok: %-15s (%ss)\n' "$step" "$dur"
    else
      dur="$([[ -f "$tmp/$step.dur" ]] && cat "$tmp/$step.dur" || echo '?')"
      printf 'fail: %-13s (%ss; recovery classification deferred until siblings settle)\n' \
        "$step" "$dur"
      failed_lane_indices+=("$idx")
    fi
  done

  # Only recover after every initially launched lane is settled. Cleaning an
  # implicated package any earlier can invalidate build-script outputs while a
  # sibling is still compiling or consuming them. The classifier restricts
  # recovery to missing outputs inside this gate's active Cargo build directory;
  # ordinary failures receive no cleanup or retry, and malformed paths fail
  # closed.
  for idx in "${failed_lane_indices[@]}"; do
    local step="${steps[$idx]}"
    local runner="${runners[$idx]}"
    local first_log="$tmp/$step.first-attempt.log"
    local cleaned="$tmp/$step.cleaned-packages"
    local fallback_state="$tmp/$step.target-cleared"
    local recovery_target_dir="$CARGO_TARGET_DIR"
    local recovery_build_dir="$CARGO_BUILD_BUILD_DIR"
    if [[ "$step" == "package-audit" ]]; then
      recovery_target_dir="$HARN_PACKAGE_VERIFY_TARGET_DIR"
      recovery_build_dir="$HARN_PACKAGE_VERIFY_BUILD_DIR"
    fi

    : > "$cleaned"
    rm -f "$fallback_state"

    # Each round cleans only what earlier rounds have not already cleaned, so a
    # cache holding several decayed packages is repaired across rounds instead
    # of spending one retry on the first package Cargo happened to report.
    local round=0 recovery_status=0 step_recovered=0
    while [[ "$round" -lt "$RELEASE_GATE_STALE_RECOVERY_ROUNDS" ]]; do
      recovery_status=0
      release_gate_recover_stale_out_dir_round \
        "$step" "$tmp/$step.log" "$cleaned" "$fallback_state" \
        "$recovery_target_dir" "$recovery_build_dir" || recovery_status=$?
      if [[ "$recovery_status" -ne 0 && "$recovery_status" -ne 3 ]]; then
        break
      fi

      # Preserve the pre-recovery diagnostics once a retry is actually going to
      # happen; `run_step` overwrites the lane log in place. A lane that never
      # recovers has only one attempt, and reprinting it as a "first attempt"
      # would just duplicate the terminal log.
      if [[ "$round" -eq 0 ]]; then
        cp "$tmp/$step.log" "$first_log"
      fi
      round=$(( round + 1 ))
      printf 'recovery: retrying %s (round %s of %s) after every initial audit lane settled\n' \
        "$step" "$round" "$RELEASE_GATE_STALE_RECOVERY_ROUNDS"
      run_step "$step" "$runner" &
      local retry_pid
      retry_pid=$!
      local retry_dur
      if wait "$retry_pid"; then
        retry_dur="$([[ -f "$tmp/$step.dur" ]] && cat "$tmp/$step.dur" || echo '?')"
        printf 'ok: %-15s (%ss retry)\n' "$step" "$retry_dur"
        echo "recovery: $step succeeded after stale build-script cleanup"
        step_recovered=1
        break
      fi
      retry_dur="$([[ -f "$tmp/$step.dur" ]] && cat "$tmp/$step.dur" || echo '?')"
      printf 'fail: %-13s (%ss retry %s)\n' "$step" "$retry_dur" "$round"
      recovery_status=0
    done

    if [[ "$step_recovered" -eq 1 ]]; then
      continue
    fi
    release_gate_report_stale_recovery_failure "$step" "$recovery_status"
    failed=1
    failed_steps+=("$step")
  done

  # The performance ratchet measures wall and CPU time, so running it beside
  # rust-audit's workspace build/tests and package-audit's extracted-crate
  # builds measures our own fanout rather than Harn. Mirror audit_gates.sh:
  # settle every functional lane first, then collect performance evidence on
  # the otherwise-idle runner.
  if [[ "$needs_harn_performance" -eq 1 ]]; then
    local performance_step="harn-performance"
    local performance_dur=""
    printf 'log: %-15s (%s)\n' "$performance_step" "$tmp/$performance_step.log"
    steps+=("$performance_step")
    if run_step "$performance_step" run_harn_performance_audit; then
      performance_dur="$([[ -f "$tmp/$performance_step.dur" ]] && cat "$tmp/$performance_step.dur" || echo '?')"
      printf 'ok: %-15s (%ss)\n' "$performance_step" "$performance_dur"
    else
      performance_dur="$([[ -f "$tmp/$performance_step.dur" ]] && cat "$tmp/$performance_step.dur" || echo '?')"
      printf 'fail: %-13s (%ss)\n' "$performance_step" "$performance_dur"
      failed=1
      failed_steps+=("$performance_step")
    fi
  fi

  release_gate_print_failed_lane_summary() {
    local heading="$1"
    echo ""
    echo "$heading"
    # Report exactly the lanes that are still failing. Selecting them by
    # scanning logs for error text hid lanes killed by a signal, whose last
    # line is the shell's own `Killed: 9` notice and matches no error pattern.
    for step in ${failed_steps[@]+"${failed_steps[@]}"}; do
      local log="$tmp/$step.log"
      echo ""
      # A `time_phase` sub-step that opened (`  -> label ...`) without a
      # matching close (`  <- label (Ns)`) is the one that failed. This
      # pinpoints e.g. "grammar-audit / verify_tree_sitter_parse" instead of
      # just "grammar-audit".
      local failing_sub=""
      if [[ -f "$log" ]]; then
        failing_sub="$(awk '
          /^  -> / { sub(/^  -> /, ""); sub(/ \.\.\.$/, ""); open=$0 }
          /^  <- / { open="" }
          END { if (open != "") print open }
        ' "$log")"
      fi
      if [[ -n "$failing_sub" ]]; then
        echo ">>> ${step} / ${failing_sub}  <<< (failing sub-step)"
      else
        echo ">>> ${step}  <<<"
      fi
      echo "    $(release_gate_lane_exit_description "$tmp/$step.rc")"
      if [[ -f "$log" ]]; then
        echo "    last 40 lines of $step.log:"
        tail -n 40 "$log" | sed 's/^/      /'
      else
        echo "    no log was written"
      fi
    done
  }

  if [[ "$failed" -ne 0 ]]; then
    # Put the summary first for people reading the complete audit. Repeat the
    # same bounded summary at the end because hosted command adapters and CI
    # surfaces often show only the tail of a multi-megabyte Cargo log.
    release_gate_print_failed_lane_summary \
      "=== RELEASE AUDIT FAILED — failing step(s) ==="

    # ── Full logs AFTER the summary, for deep debugging. ──
    echo ""
    echo "=== Full failed-audit logs ==="
    for step in "${steps[@]}"; do
      if [[ -f "$tmp/$step.first-attempt.log" ]] && [[ -s "$tmp/$step.first-attempt.log" ]]; then
        echo "--- $step (first attempt, before stale-output recovery) ---"
        cat "$tmp/$step.first-attempt.log"
        echo ""
      fi
      if [[ -f "$tmp/$step.log" ]] && [[ -s "$tmp/$step.log" ]]; then
        echo "--- $step (terminal attempt) ---"
        cat "$tmp/$step.log"
        echo ""
      fi
    done
    release_gate_print_failed_lane_summary \
      "=== RELEASE AUDIT FAILURE RECAP — failing step(s) ==="
    rm -rf "$tmp"
    exit 1
  fi

  if [[ "$PRESERVE_AUDIT_TMP" -ne 1 ]]; then
    rm -rf "$tmp"
  fi
  local audit_elapsed=$(( $(date +%s) - audit_started ))
  echo "=== Audit complete (${audit_elapsed}s) ==="
}

cmd_prepare() {
  local bump=""
  local preid=""
  local allow_dirty=0
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --bump)
        bump="${2:-}"
        shift 2
        ;;
      --preid)
        preid="${2:-}"
        shift 2
        ;;
      --allow-dirty)
        # Used by `release_ship.sh --prepare`, which runs from a release
        # branch that already has the human's authored release content
        # (changelog, code, docs) staged or unstaged. The version-bump
        # write below is additive; we don't need a clean tree.
        allow_dirty=1
        shift
        ;;
      *)
        echo "error: unknown prepare arg: $1"
        usage
        exit 1
        ;;
    esac
  done
  if [[ -z "$bump" ]]; then
    echo "error: prepare requires --bump KIND"
    exit 1
  fi
  if [[ "$bump" != "patch" || -n "$preid" ]]; then
    echo "error: stable releases strip the declared X.Y.Z-dev target; use --bump patch without --preid"
    exit 1
  fi
  if [[ "$allow_dirty" -eq 0 ]]; then
    require_clean_tree
  fi
  (
    local stable_tool_dir current next
    stable_tool_dir="$(mktemp -d)"
    trap 'rm -rf "$stable_tool_dir"' EXIT

    # Build and snapshot the exact-source generator before rewriting Harn's own
    # workspace version, so the version rewrite costs no second shared-graph
    # compilation. The generated bytes still describe the bumped tree: the
    # generator reads the post-bump manifest and stamps that version into every
    # artifact header, rather than the version it was itself built at. Anything
    # the generator reads from its own binary instead of the tree would ship a
    # payload the released runtime rejects (see #6084).
    release_gate_snapshot_prepare_aot_generator "$stable_tool_dir"
    unset HARN_RELEASE_CLI_AOT_GEN_BIN
    # Same reasoning, for every prepare-time `.harn` tool: snapshot one
    # exact-source CLI while the tree still matches the audited candidate, so
    # `release_metadata current/release-target/apply`, protocol-fixture syncing, and
    # artifact dumping all run on one binary instead of recompiling the runtime
    # graph after each metadata mutation.
    release_gate_snapshot_prepare_tools_cli "$stable_tool_dir"
    unset HARN_RELEASE_TOOLS_BIN
    current="$(current_version)"
    next="$(next_version "$bump" "$preid")"
    bump_version "$next" "$bump" "$preid"
    # The prepare-time tools now run on the snapshot above, so nothing here
    # resolves a binary through Cargo. These stay set for any Cargo work the
    # steps below still reach (`reconcile_cargo_lock`, `make gen-cli-aot`),
    # keeping it isolated from incremental and sccache state.
    # `CARGO_BUILD_RUSTC_WRAPPER=` is required because Harn's checked-in
    # `.cargo/config.toml` otherwise still applies sccache.
    export CARGO_INCREMENTAL=0
    export RUSTC_WRAPPER=
    export CARGO_BUILD_RUSTC_WRAPPER=
    export SCCACHE_DISABLE=1

    reconcile_cargo_lock

    harn_cmd run scripts/sync_protocol_fixture_runtime_versions.harn -- --from "$current" --to "$next"
    # Artifact contents come from the already-audited source checkout. Stamp the
    # selected release version explicitly so a Cargo.toml-only rewrite does not
    # force a second full CLI build.
    harn_cmd dump-protocol-artifacts --artifact-version "$next"
    # The package/release payload is intentionally ignored rather than committed.
    # Generate it once after the version bump with the stable pre-bump generator;
    # later audit/package steps verify the same target-independent bytes.
    HARN_CLI_AOT_GEN_BIN="$RELEASE_PREPARE_AOT_BIN" \
      HARN_CLI_AOT_ARTIFACT_VERSION="$next" \
      make gen-cli-aot
    echo "Version updated: $current -> $next"
    echo "Next steps:"
    echo "  1. Review docs/release notes diff"
    echo "  2. Commit on a release/v$next branch: git commit -am 'Release v$next'"
    echo "  3. Push the signed v$next tag at the pinned release commit"
    echo "  4. Open the Release v$next PR and enable auto-merge"
    echo "  5. Let the tag-triggered publish and binary workflows finish"
    echo "  6. Require Release smoke against the published artifacts:"
    echo "       ./scripts/check_release_smoke.sh v$next"
  )
}

cmd_publish() {
  local dry_run=""
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --dry-run)
        dry_run="--dry-run"
        shift
        ;;
      *)
        echo "error: unknown publish arg: $1"
        usage
        exit 1
        ;;
    esac
  done
  if [[ -z "$dry_run" ]]; then
    require_clean_tree
  fi
  "$PUBLISH_SCRIPT" ${dry_run}
  local version
  version="$(current_version)"
  if [[ -n "$dry_run" ]]; then
    echo "Publish dry run complete for v$version"
    return
  fi
  echo "Publish phase complete for v$version"
  echo "Follow-up / verification checklist:"
  echo "  Ensure tag v$version has been pushed from the merge-queue-approved main commit"
  echo "  Review changelog-backed GitHub release notes"
  echo "  Wait for Build release binaries to finalize the GitHub release (7 assets)"
  echo "  Require Release smoke to pass against the published artifacts:"
  echo "    ./scripts/check_release_smoke.sh v$version"
}

cmd_notes() {
  local version=""
  local output=""
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --version)
        version="${2:-}"
        shift 2
        ;;
      --output)
        output="${2:-}"
        shift 2
        ;;
      *)
        echo "error: unknown notes arg: $1"
        usage
        exit 1
        ;;
    esac
  done
  if [[ -z "$version" ]]; then
    version="$(current_version)"
  fi
  if [[ -n "$output" ]]; then
    harn_cmd run scripts/render_release_notes.harn -- --version "$version" --output "$output"
    echo "Rendered release notes for ${version#v} -> $output"
  else
    harn_cmd run scripts/render_release_notes.harn -- --version "$version"
  fi
}

cmd_full() {
  local dry_run=""
  local bump=""
  local preid=""
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --dry-run)
        dry_run="--dry-run"
        shift
        ;;
      --bump)
        bump="${2:-}"
        shift 2
        ;;
      --preid)
        preid="${2:-}"
        shift 2
        ;;
      *)
        echo "error: unknown full arg: $1"
        usage
        exit 1
        ;;
    esac
  done
  PRESERVE_AUDIT_TMP=1
  trap cleanup_preserved_audit_tmp EXIT
  cmd_audit
  local prepare_args=(--bump "${bump:-patch}")
  if [[ -n "$preid" ]]; then
    prepare_args+=(--preid "$preid")
  fi
  cmd_prepare "${prepare_args[@]}"
  cmd_publish ${dry_run}
  cleanup_preserved_audit_tmp
  trap - EXIT
}

case "${1:-}" in
  audit)
    shift
    cmd_audit "$@"
    ;;
  prepare)
    shift
    cmd_prepare "$@"
    ;;
  publish)
    shift
    cmd_publish "$@"
    ;;
  notes)
    shift
    cmd_notes "$@"
    ;;
  full)
    shift
    cmd_full "$@"
    ;;
  *)
    usage
    exit 1
    ;;
esac
