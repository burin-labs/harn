#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${HARN_RELEASE_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
cd "$ROOT_DIR"
RELEASE_GATE_SCRIPT="${HARN_RELEASE_GATE_SCRIPT:-./scripts/release_gate.sh}"

# ── Timing instrumentation ──────────────────────────────────────────────
# Every `=== step ===` banner goes through `log_step`, which stamps the
# wall clock, elapsed since script start, and elapsed since the previous
# step. Release steps that invoke child scripts or hooks (audit, publish,
# pre-push `make test`) are often measured in minutes, and the delta
# between steps answers "where is the long pole?" without needing a
# separate profiler. Output format:
#
#   === audit ===  (t+00:01:23  Δ00:00:00)
#   === publish dry-run ===  (t+00:04:07  Δ00:02:44)
#
# Uses nanosecond `date +%s%N` when available (Linux), falls back to
# whole-second `date +%s` on macOS where coreutils is not installed.
SHIP_START_NS="$(date +%s)000000000"
LAST_STEP_NS="$SHIP_START_NS"

_ship_now_ns() {
  # `date +%s%N` is a GNU extension; BSD/macOS date truncates the `%N`.
  # If `%N` comes through unchanged we assume whole-second precision.
  local raw
  raw="$(date +%s%N)"
  case "$raw" in
    *N) printf '%s000000000\n' "$(date +%s)" ;;
    *)  printf '%s\n' "$raw" ;;
  esac
}

_ship_fmt_ns() {
  local ns="$1"
  local total_ms=$(( ns / 1000000 ))
  local total_s=$(( total_ms / 1000 ))
  local ms=$(( total_ms % 1000 ))
  local h=$(( total_s / 3600 ))
  local m=$(( (total_s % 3600) / 60 ))
  local s=$(( total_s % 60 ))
  printf '%02d:%02d:%02d.%03d' "$h" "$m" "$s" "$ms"
}

log_step() {
  local name="$1"
  local now
  now="$(_ship_now_ns)"
  local elapsed=$(( now - SHIP_START_NS ))
  local delta=$(( now - LAST_STEP_NS ))
  LAST_STEP_NS="$now"
  printf '=== %s === (t+%s  Δ%s  wall %s)\n' \
    "$name" \
    "$(_ship_fmt_ns "$elapsed")" \
    "$(_ship_fmt_ns "$delta")" \
    "$(date '+%H:%M:%S')"
}

usage() {
  cat <<'EOF'
Usage:
  ./scripts/release_ship.sh --prepare --bump patch|minor|major [--skip-audit] [--skip-dry-run]  # release_harn.harn only
  ./scripts/release_ship.sh --bump patch|minor|major [--skip-dry-run] [--base main]   # recovery
  ./scripts/release_ship.sh --finalize [--skip-dry-run] [--reaudit] [--notes-output path] [--skip-github-release] [--base main]

Merge-queue-safe release sequence for a prepared Harn release.

==============================================================================
DEFAULT FLOW (one human PR, then bot finalizes)
==============================================================================

  1. Branch off main at a frozen commit (release_harn.harn does this
     automatically) and write the release content:
       git checkout -b release/vX.Y.Z <pin-sha>
       # author code/docs changes
       # add `## vX.Y.Z` heading at the top of CHANGELOG.md

  2. Stage release-content files but do NOT commit yet.

  3. Run prepare-here, which audits, dry-run-publishes, bumps
     Cargo.toml/Cargo.lock, regenerates derived files, and stages
     everything ready for a single commit:
       harn run --no-sandbox ~/projects/harn-bump-fleet/release_harn.harn -- --mode ship-pr --agent --yes-live-release

  4. The release_harn.harn harness commits, pushes, tags, pushes the tag,
     and opens the PR in that order so the tag is on origin BEFORE the PR
     is opened:
       git commit -m "Release vX.Y.Z"
       git push -u origin release/vX.Y.Z
       git tag -a vX.Y.Z -m "Release vX.Y.Z"
       git push origin vX.Y.Z              # ← triggers publish-release.yml
       gh pr create

  5. publish-release.yml fires on the tag push, checks out the tagged
     commit (detached), and finalizes crates.io. The
     build-release-binaries cascade creates the GitHub release with
     binary artifacts from the same pinned commit. The Release PR
     remains as paperwork; auto-merge can land it whenever, even if
     main has moved on. The post-merge push to main no-ops (no drift,
     since Cargo.toml on main now matches the tag).

==============================================================================
PREPARE MODE
==============================================================================

  - Implementation detail used by release_harn.harn. Standalone prepare is
    rejected because the tag must be pushed before the Release PR enters the
    merge queue.
  - Runs from a non-main branch with the release content already authored.
  - Detects bump type via --bump and confirms it matches the CHANGELOG
    top entry (CHANGELOG must be at the next vX.Y.Z heading already).
  - Runs the full audit (skip with --skip-audit) and publish dry-run
    (skip with --skip-dry-run) so failures surface before push.
  - Bumps Cargo.toml + crates/*/Cargo.toml + Cargo.lock to vX.Y.Z.
  - Regenerates derived files (`docs/src/language-spec.md`,
    `docs/theme/harn-keywords.js`).
  - Stages everything; the human commits and pushes.

==============================================================================
LEGACY BUMP MODE (recovery only)
==============================================================================

  Pre-consolidation behavior kept for the recovery workflow_dispatch
  path on .github/workflows/bump-release.yml. Runs from main, opens a
  "Bump version to X.Y.Z" PR from a release/vX.Y.Z branch. Use only
  when a "Prepare vX.Y.Z release"-style commit landed on main without
  the consolidated bump (the workflow flips itself out of the default
  push-trigger to make this an explicit recovery action).

==============================================================================
FINALIZE MODE
==============================================================================

  Trusts the merge-queue CI that just landed the consolidated PR (CI
  runs the full audit set: cargo fmt/clippy/tests, conformance,
  highlight + language-spec sync checks, docs-snippets, trigger
  quickref + examples, verify_release_metadata, portal lint+build).

  Two checkout shapes are supported, picked automatically:

    a. Detached at vX.Y.Z (the canonical "tag is truth" path) — fired
       by publish-release.yml on tag push, OR a local recovery run
       after manually `git checkout v0.7.53`. Skips the main
       fast-forward and publishes the already-tagged commit, so
       crates.io content, release notes, and the tag stay aligned
       even if main has moved on.

    b. On main with drift — legacy/recovery path. Fast-forwards local
       main, tags HEAD as vX.Y.Z (must match Cargo.toml's workspace
       version), pushes the tag, and publishes. Used when a release
       was authored without release_harn.harn or the tag-push trigger
       failed and we're recovering via push:main drift.

  Steps in either case:

  1. Builds the portal frontend (needed for the cargo-publish include).
  2. Runs the publish dry-run as a quick pre-publish sanity check.
  3. Creates the tag vX.Y.Z at HEAD (no-op if it already exists at HEAD).
  4. Pushes the tag (no-op if origin already has it).
  5. Runs cargo publish to upload crates to crates.io.
  6. Renders changelog-backed release notes.
  7. Creates/updates a GitHub release with the rendered notes, unless
     --skip-github-release is passed because another workflow owns the
     release page and binary artifacts.

  Set RELEASE_FINALIZE_REAUDIT=1 (or pass --reaudit) to opt back into
  the full release-gate audit before finalizing — useful when running
  --finalize locally after manual repo edits.

==============================================================================
ENVIRONMENT VARIABLES
==============================================================================

  HARN_BOOTSTRAP_NEW_CRATES=1
    First-release bootstrap mode for a brand-new workspace crate that
    an already-published crate now depends on. Skips the publish
    dry-run and tells verify_crate_packages.sh to skip package checks
    for downstream crates that fail when a path-dep crate isn't on
    crates.io yet). The real publish later uses
    `cargo publish --workspace`, which orders intra-workspace deps
    correctly. See harn#609.

  RELEASE_FINALIZE_REAUDIT=1
    Force --finalize to re-run the full release-gate audit. Defaults
    off — merge-queue CI already proved the same gates a few minutes
    ago. Use when finalizing locally after edits.

  HARN_RELEASE_HARNESS=1
    Required for --prepare. Set by harn-bump-fleet/release_harn.harn after
    it pins the release commit and before it pushes the vX.Y.Z tag.
EOF
}

require_clean_tree() {
  local status
  status="$(git status --porcelain --untracked-files=normal)"
  if [[ -n "$status" ]]; then
    echo "error: working tree is dirty"
    printf '%s\n' "$status"
    echo "hint: commit, stash, or discard changes before running release_ship.sh from main"
    exit 1
  fi
}

current_version() {
  python3 - <<'PY'
from pathlib import Path
import re
text = Path("Cargo.toml").read_text()
m = re.search(r'^version = "([^"]+)"', text, re.M)
print(m.group(1) if m else "")
PY
}

next_version() {
  local bump="$1"
  python3 - "$bump" <<'PY'
from pathlib import Path
import re, sys
bump = sys.argv[1]
text = Path("Cargo.toml").read_text()
m = re.search(r'^version = "([^"]+)"', text, re.M)
if not m:
    raise SystemExit("missing workspace version")
major, minor, patch = map(int, m.group(1).split("."))
if bump == "major":
    major, minor, patch = major + 1, 0, 0
elif bump == "minor":
    minor, patch = minor + 1, 0
elif bump == "patch":
    patch += 1
else:
    raise SystemExit(f"unsupported bump: {bump}")
print(f"{major}.{minor}.{patch}")
PY
}

debug_harn_binary() {
  local target_dir="${CARGO_TARGET_DIR:-}"
  if [[ -z "$target_dir" ]]; then
    target_dir="$(cargo metadata --format-version=1 --no-deps \
      | python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')"
  fi
  local suffix=""
  case "${OS:-$(uname -s)}" in
    Windows_NT|MINGW*|MSYS*|CYGWIN*) suffix=".exe" ;;
  esac
  printf '%s/debug/harn%s\n' "$target_dir" "$suffix"
}

export_warmed_harn_bin() {
  if [[ -n "${HARN_BIN:-}" ]]; then
    return 0
  fi
  local harn_bin
  harn_bin="$(debug_harn_binary)"
  if [[ -x "$harn_bin" ]]; then
    export HARN_BIN="$harn_bin"
    printf 'Reusing warmed HARN_BIN: %s\n' "$HARN_BIN"
  fi
}

disable_prepare_cargo_cache_wrappers() {
  export CARGO_INCREMENTAL=0
  export RUSTC_WRAPPER=
  export CARGO_BUILD_RUSTC_WRAPPER=
  export SCCACHE_DISABLE=1
}

require_base_branch() {
  local base="$1"
  local branch
  branch="$(git branch --show-current)"
  if [[ "$branch" != "$base" ]]; then
    echo "error: release_ship.sh must run from $base; current branch is ${branch:-detached}"
    echo "hint: wait for release-content/version-bump PRs to land, then sync $base"
    exit 1
  fi
  git fetch origin "$base" --quiet
  local local_head remote_head
  local_head="$(git rev-parse HEAD)"
  remote_head="$(git rev-parse "origin/$base")"
  if [[ "$local_head" != "$remote_head" ]]; then
    echo "error: local $base is not up to date with origin/$base"
    echo "hint: git pull --ff-only origin $base"
    exit 1
  fi
}

# Sync local base branch to origin/base instead of asserting strict
# equality. Used by --finalize where the GHA runner clones main early
# in setup, then sits through ~30s of toolchain installs before this
# script runs — main can move during that window. The bump intent is
# already in main's history regardless of where HEAD is now, so the
# right move is to fast-forward the local clone and tag whichever
# commit Cargo.toml's version is set on.
sync_base_branch() {
  local base="$1"
  local branch
  branch="$(git branch --show-current)"
  if [[ "$branch" != "$base" ]]; then
    echo "error: release_ship.sh --finalize must run from $base; current branch is ${branch:-detached}"
    exit 1
  fi
  git fetch origin "$base" --quiet
  local local_head remote_head
  local_head="$(git rev-parse HEAD)"
  remote_head="$(git rev-parse "origin/$base")"
  if [[ "$local_head" != "$remote_head" ]]; then
    echo "Local $base behind origin/$base; fast-forwarding."
    echo "  was:  $local_head"
    echo "  now:  $remote_head"
    git pull --ff-only origin "$base" --quiet
  fi
  # Re-read Cargo.toml after the fast-forward — if main has continued
  # past the bump and someone else has already bumped to a newer
  # version, abort rather than silently tagging the wrong commit.
  local fresh_version
  fresh_version="$(current_version)"
  if [[ -n "${EXPECTED_VERSION:-}" && "$fresh_version" != "$EXPECTED_VERSION" ]]; then
    echo "error: Cargo.toml version moved from $EXPECTED_VERSION to $fresh_version while finalizing"
    echo "hint: re-trigger publish-release.yml — it'll detect drift for the new version"
    exit 1
  fi
}

require_existing_release_tag_checkout() {
  local version="$1"
  local tag="v$version"
  local branch
  branch="$(git branch --show-current)"
  if [[ -n "$branch" ]]; then
    echo "error: release_ship.sh --finalize must run from $BASE_BRANCH or detached at $tag; current branch is $branch"
    exit 1
  fi
  if ! git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
    echo "error: release_ship.sh --finalize is detached, but $tag does not exist locally"
    exit 1
  fi
  local tag_commit head_commit
  tag_commit="$(git rev-list -n 1 "$tag")"
  head_commit="$(git rev-parse HEAD)"
  if [[ "$tag_commit" != "$head_commit" ]]; then
    echo "error: release_ship.sh --finalize is detached at $head_commit, but $tag points to $tag_commit"
    echo "hint: checkout $BASE_BRANCH for a normal finalize, or checkout $tag for manual tag-publish recovery"
    exit 1
  fi
  echo "Finalize recovery from existing tag $tag at $head_commit"
}

require_release_branch() {
  local base="$1"
  local branch
  branch="$(git branch --show-current)"
  if [[ -z "$branch" ]]; then
    echo "error: detached HEAD; create a release branch first (git checkout -b release/vX.Y.Z)"
    exit 1
  fi
  if [[ "$branch" == "$base" ]]; then
    echo "error: --prepare must run from a release branch, not $base"
    echo "hint: git checkout -b release/vX.Y.Z"
    exit 1
  fi
}

require_release_harness_prepare() {
  if [[ "${HARN_RELEASE_HARNESS:-0}" == "1" ]]; then
    return 0
  fi
  echo "error: release_ship.sh --prepare is only supported through release_harn.harn"
  echo "hint: run: harn run --no-sandbox ~/projects/harn-bump-fleet/release_harn.harn -- --mode ship-pr --agent --yes-live-release"
  echo "hint: tag-first is mandatory; standalone prepare can open a racy Release PR without a pre-pushed tag"
  exit 1
}

# Verify the top CHANGELOG.md heading matches the expected next version.
# The human is expected to have authored "## vX.Y.Z" before running prepare.
#
# Convenience: if the first H2 is `## Unreleased` (the convention for
# accumulating bullets between releases), promote it to `## v$expected`
# in-place rather than failing after the audit has already burned ~7
# minutes of wall time. The `git add -u` in the staging step picks the
# rename up; if --prepare is later cancelled the rename is harmless.
require_changelog_top_matches() {
  local expected="$1"
  python3 - "$expected" <<'PY'
import re
import sys
from pathlib import Path

expected = sys.argv[1]
path = Path("CHANGELOG.md")
text = path.read_text()
lines = text.splitlines(keepends=True)
top_index = None
for i, line in enumerate(lines):
    if re.match(r"^## ", line):
        top_index = i
        break

if top_index is None:
    sys.stderr.write("error: CHANGELOG.md has no '## vX.Y.Z' heading\n")
    sys.exit(1)

top = lines[top_index].rstrip("\n").rstrip("\r")
m = re.match(r"^## v(\d+\.\d+\.\d+)\s*$", top)
if m:
    if m.group(1) == expected:
        sys.exit(0)
    sys.stderr.write(
        f"error: CHANGELOG.md top heading is v{m.group(1)} but --bump implies v{expected}\n"
        f"hint: edit CHANGELOG.md to add '## v{expected}' as the new top entry, then re-run\n"
    )
    sys.exit(1)

if re.match(r"^## Unreleased\s*$", top):
    lines[top_index] = f"## v{expected}\n"
    path.write_text("".join(lines))
    print(f"note: renamed CHANGELOG.md top heading '## Unreleased' -> '## v{expected}'")
    sys.exit(0)

sys.stderr.write(
    f"error: CHANGELOG.md top heading '{top}' is neither '## Unreleased' "
    f"nor '## v$expected'\n"
    f"hint: edit CHANGELOG.md to add '## v{expected}' as the new top entry, then re-run\n"
)
sys.exit(1)
PY
}

run_common_gates() {
  # Build the portal frontend up front so `portal-dist/` exists for every
  # downstream step. The `harn-cli` crate embeds portal-dist via `include_dir!`
  # at compile time and ships it via the crate's `include = [...]` field, so
  # both the audit (clippy + tests) and the subsequent cargo publish need the
  # real bundle on disk. portal-dist/ is gitignored — a fresh clone or CI run
  # would otherwise get the build.rs placeholder.
  log_step "Build portal frontend"
  make portal-check

  # New-crate bootstrap mode: if the prepare PR introduced a workspace
  # crate that's also depended on by an already-published crate (e.g.
  # harn-cli pulling in the brand-new harn-hostlib), cargo's
  # dependency-resolution step inside `cargo package -p harn-cli` will
  # fail looking that crate up on crates.io even with --no-verify. Skip
  # the dry-run and let `cargo publish --workspace` order the crates at
  # real-publish time. The audit's package-audit lane reads the same
  # env var via verify_crate_packages.sh and skips downstream package
  # checks that need the new crate to already exist on crates.io.
  # See harn#609 for the full failure mode.
  if [[ "${HARN_BOOTSTRAP_NEW_CRATES:-0}" == "1" ]]; then
    echo "=== HARN_BOOTSTRAP_NEW_CRATES=1: skipping publish dry-run ==="
    SKIP_DRY_RUN=1
  fi

  if [[ "$SKIP_AUDIT" -eq 0 ]]; then
    log_step "Release audit"
    "$RELEASE_GATE_SCRIPT" audit
  else
    log_step "Skipping release audit (already proved by merge-queue CI)"
  fi

  if [[ "$SKIP_DRY_RUN" -eq 0 ]]; then
    log_step "Publish dry run"
    "$RELEASE_GATE_SCRIPT" publish --dry-run
  fi
}

regenerate_derived_files() {
  log_step "Regenerate derived files"
  # Both targets are idempotent no-ops when the generated artifact is
  # already current. They exist as Makefile targets so the same
  # canonical command works locally and from CI.
  make sync-language-spec
  make gen-highlight
}

open_bump_pr() {
  local base="$1"
  local previous="$2"
  local next="$3"
  local branch="release/v$next"
  local tag="v$next"

  if git show-ref --verify --quiet "refs/heads/$branch"; then
    echo "error: local branch already exists: $branch"
    exit 1
  fi

  log_step "Create bump branch"
  git switch -c "$branch"

  export_warmed_harn_bin
  log_step "Version bump"
  "$RELEASE_GATE_SCRIPT" prepare --bump "$BUMP"
  local actual_next
  actual_next="$(current_version)"
  if [[ "$actual_next" != "$next" ]]; then
    echo "error: expected version $next, got $actual_next"
    exit 1
  fi

  log_step "Commit version bump"
  git add Cargo.toml Cargo.lock crates/*/Cargo.toml spec/acp-registry/harn/agent.json
  git commit -m "Bump version to $next"

  log_step "Push bump branch"
  git push -u origin "$branch"

  local body_file
  body_file="$(mktemp)"
  cat >"$body_file" <<EOF
Automated version-bump PR for $tag.

Release gates completed before opening this PR:

- ./scripts/release_gate.sh audit
- ./scripts/release_gate.sh publish --dry-run, unless --skip-dry-run was passed

After this PR lands through the merge queue, finalize from an up-to-date $base:

\`\`\`bash
./scripts/release_ship.sh --finalize
\`\`\`
EOF

  if command -v gh &>/dev/null; then
    log_step "Open bump PR"
    local pr_url
    pr_url="$(gh pr create \
      --base "$base" \
      --head "$branch" \
      --title "Bump version to $next" \
      --body-file "$body_file")"
    echo "$pr_url"

    log_step "Label bump PR"
    gh pr edit "$pr_url" --add-label "no-changelog-needed"

    log_step "Enable bump PR auto-merge"
    gh pr merge "$pr_url" --auto --squash
  else
    echo "warning: gh CLI not found — skipping PR creation"
    echo "hint: open a PR from $branch into $base titled 'Bump version to $next'"
  fi
  rm -f "$body_file"

  log_step "Bump PR ready"
  TOTAL_NS=$(( $(_ship_now_ns) - SHIP_START_NS ))
  echo ""
  echo "Release bump PR ready:"
  echo "  Previous version: $previous"
  echo "  Next version:     $next"
  echo "  Base branch:      $base"
  echo "  Bump branch:      $branch"
  echo "  Tag after merge:  $tag"
  echo "  Auto-merge:       enabled"
  echo "  Total wall time:  $(_ship_fmt_ns "$TOTAL_NS")"
  echo "  Finalize after merge queue lands it: ./scripts/release_ship.sh --finalize"
}

prepare_here() {
  local previous="$1"
  local next="$2"
  local branch
  branch="$(git branch --show-current)"

  # Run audit + dry-run from the dirty release branch. The audit's
  # cargo steps don't care about uncommitted changes; the publish
  # dry-run already auto-detects dirtiness and falls back to
  # --allow-dirty (see scripts/publish.sh).
  run_common_gates

  export_warmed_harn_bin
  log_step "Version bump (in place)"
  "$RELEASE_GATE_SCRIPT" prepare --bump "$BUMP" --allow-dirty
  local actual_next
  actual_next="$(current_version)"
  if [[ "$actual_next" != "$next" ]]; then
    echo "error: expected version $next, got $actual_next"
    exit 1
  fi

  # `release_gate.sh prepare` disables cargo/sccache state for post-bump
  # one-shot rebuilds. That export does not survive the subprocess boundary,
  # so re-apply it for the cargo invocations `regenerate_derived_files` runs in
  # this shell.
  disable_prepare_cargo_cache_wrappers
  regenerate_derived_files

  log_step "Stage release content"
  # Stage the version bump deterministically and then sweep tracked
  # changes (changelog, code, docs, generated mirrors) so the human's
  # next `git commit` captures the whole release in one shot.
  git add Cargo.toml Cargo.lock crates/*/Cargo.toml
  git add docs/src/language-spec.md docs/src/spec/language docs/src/SUMMARY.md docs/theme/harn-keywords.js
  git add -u

  log_step "Prepare-here ready"
  TOTAL_NS=$(( $(_ship_now_ns) - SHIP_START_NS ))
  echo ""
  echo "Release content staged on $branch:"
  echo "  Previous version: $previous"
  echo "  Next version:     $next"
  echo "  Total wall time:  $(_ship_fmt_ns "$TOTAL_NS")"
  echo ""
  echo "Next steps:"
  echo "  git status                                # review staged changes"
  echo "  git commit -m \"Release v$next\""
  echo "  git push -u origin $branch"
  echo "  gh pr create --title \"Release v$next\" --body \"...\""
  echo ""
  echo "After the PR lands through the merge queue, the publish-release"
  echo "workflow auto-fires on tag drift and ships v$next."
}

tag_exists() {
  local tag="$1"
  git rev-parse -q --verify "refs/tags/$tag" >/dev/null
}

ensure_tag_at_head() {
  local tag="$1"
  if tag_exists "$tag"; then
    local tag_commit head_commit
    tag_commit="$(git rev-list -n 1 "$tag")"
    head_commit="$(git rev-parse HEAD)"
    if [[ "$tag_commit" != "$head_commit" ]]; then
      echo "error: $tag already exists at $tag_commit, but HEAD is $head_commit"
      exit 1
    fi
    echo "Tag already exists at HEAD: $tag"
  else
    # Some developer machines set tag.gpgSign/tag.forceSignAnnotated. In that
    # configuration even plain `git tag <name>` can become an annotated signed
    # tag and open $EDITOR for the message. Finalization must be noninteractive,
    # so provide the stable release message explicitly.
    git tag -m "Release $tag" "$tag"
  fi
}

BUMP="patch"
SKIP_DRY_RUN=0
SKIP_AUDIT=0
MODE="bump-pr"
BASE_BRANCH="main"
NOTES_OUTPUT=""
SKIP_GITHUB_RELEASE=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --bump)
      BUMP="${2:-}"
      shift 2
      ;;
    --prepare)
      MODE="prepare-here"
      shift
      ;;
    --finalize)
      MODE="finalize"
      shift
      ;;
    --base)
      BASE_BRANCH="${2:-}"
      shift 2
      ;;
    --skip-dry-run)
      SKIP_DRY_RUN=1
      shift
      ;;
    --skip-audit)
      SKIP_AUDIT=1
      shift
      ;;
    --reaudit)
      # --finalize defaults to skipping the audit (merge-queue CI just
      # proved the same set). --reaudit forces it back on.
      SKIP_AUDIT=0
      RELEASE_FINALIZE_REAUDIT=1
      shift
      ;;
    --notes-output)
      NOTES_OUTPUT="${2:-}"
      shift 2
      ;;
    --skip-github-release)
      SKIP_GITHUB_RELEASE=1
      shift
      ;;
    --no-push)
      echo "error: --no-push was removed from release_ship.sh"
      echo "hint: use ./scripts/release_gate.sh prepare/publish/notes for manual piecewise work"
      exit 1
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown arg: $1"
      usage
      exit 1
      ;;
  esac
done

case "$BUMP" in
  patch|minor|major) ;;
  *)
    echo "error: --bump must be patch, minor, or major"
    exit 1
    ;;
esac

# Mode-specific guards. Each mode runs against a different baseline:
#   prepare-here: feature branch with dirty tree (release content authored)
#   bump-pr:      clean main, opens recovery release branch
#   finalize:     clean main with Cargo.toml ahead of latest tag
if [[ "$MODE" == "prepare-here" ]]; then
  require_release_harness_prepare
  require_release_branch "$BASE_BRANCH"
elif [[ "$MODE" == "finalize" ]]; then
  require_clean_tree
  EXPECTED_VERSION="$(current_version)"
  if [[ "$(git branch --show-current)" == "$BASE_BRANCH" ]]; then
    sync_base_branch "$BASE_BRANCH"
  else
    require_existing_release_tag_checkout "$EXPECTED_VERSION"
  fi
  # Trust the merge-queue CI by default; opt-in re-audit via env var or
  # --reaudit flag.
  if [[ "${RELEASE_FINALIZE_REAUDIT:-0}" != "1" ]]; then
    SKIP_AUDIT=1
  fi
else
  require_clean_tree
  require_base_branch "$BASE_BRANCH"
fi

PREVIOUS_VERSION="$(current_version)"
if [[ -z "$PREVIOUS_VERSION" ]]; then
  echo "error: failed to detect current version"
  exit 1
fi

if [[ "$MODE" == "prepare-here" ]]; then
  NEXT_VERSION="$(next_version "$BUMP")"
  if [[ "$NEXT_VERSION" == "$PREVIOUS_VERSION" ]]; then
    echo "error: --bump $BUMP would leave version unchanged at $PREVIOUS_VERSION"
    exit 1
  fi
  require_changelog_top_matches "$NEXT_VERSION"
  prepare_here "$PREVIOUS_VERSION" "$NEXT_VERSION"
  exit 0
fi

if [[ "$MODE" == "bump-pr" ]]; then
  NEXT_VERSION="$(next_version "$BUMP")"
  if [[ "$NEXT_VERSION" == "$PREVIOUS_VERSION" ]]; then
    echo "error: version did not change"
    exit 1
  fi
  run_common_gates
  open_bump_pr "$BASE_BRANCH" "$PREVIOUS_VERSION" "$NEXT_VERSION"
  exit 0
fi

NEXT_VERSION="$PREVIOUS_VERSION"
TAG="v$NEXT_VERSION"
BRANCH="$(git branch --show-current)"

run_common_gates

log_step "Tag"
ensure_tag_at_head "$TAG"

# Push the tag before cargo publish so GitHub release-binary workflows and
# downstream fetchers can start working in parallel with crates.io publication.
log_step "Push tag"
git push origin "$TAG"

log_step "Publish"
"$RELEASE_GATE_SCRIPT" publish

if [[ -z "$NOTES_OUTPUT" ]]; then
  NOTES_DIR="$ROOT_DIR/.harn/release-notes"
  mkdir -p "$NOTES_DIR"
  NOTES_OUTPUT="$(mktemp "$NOTES_DIR/notes.XXXXXX.md")"
  CLEANUP_NOTES=1
else
  CLEANUP_NOTES=0
fi

log_step "Release notes"
"$RELEASE_GATE_SCRIPT" notes --version "$TAG" --output "$NOTES_OUTPUT"
cat "$NOTES_OUTPUT"

GH_RELEASE_URL=""
if [[ "$SKIP_GITHUB_RELEASE" -eq 1 ]]; then
  log_step "GitHub release skipped"
  echo "Skipping GitHub release creation/update; binary release workflow owns release assets and notes."
elif command -v gh &>/dev/null; then
  # Create or update GitHub release with rendered notes as the LAST step, so
  # the release body reflects the final crates.io + git state. If crates.io
  # was slow, the upstream tag is already live — downstream CI will have
  # kicked off minutes ago.
  log_step "GitHub release"
  if gh release view "$TAG" &>/dev/null; then
    GH_RELEASE_URL="$(gh release edit "$TAG" --notes-file "$NOTES_OUTPUT" 2>&1)"
    echo "Updated existing release: $GH_RELEASE_URL"
  else
    GH_RELEASE_URL="$(gh release create "$TAG" --title "$TAG" --notes-file "$NOTES_OUTPUT" 2>&1)"
    echo "Created release: $GH_RELEASE_URL"
  fi
else
  echo "warning: gh CLI not found — skipping GitHub release creation"
  echo "hint: run 'gh release create $TAG --title \"$TAG\" --notes-file \"$NOTES_OUTPUT\"' manually"
fi

log_step "Release shipped"
TOTAL_NS=$(( $(_ship_now_ns) - SHIP_START_NS ))
echo ""
echo "Release shipped:"
echo "  Previous version: $PREVIOUS_VERSION"
echo "  Current version:  $NEXT_VERSION"
echo "  Branch:           $BRANCH"
echo "  Tag:              $TAG"
echo "  Notes file:       $NOTES_OUTPUT"
echo "  Total wall time:  $(_ship_fmt_ns "$TOTAL_NS")"
echo "  Push status:      pushed tag"
if [[ -n "$GH_RELEASE_URL" ]]; then
  echo "  GitHub release:   $GH_RELEASE_URL"
fi

if [[ "$CLEANUP_NOTES" -eq 1 ]]; then
  rm -f "$NOTES_OUTPUT"
fi
