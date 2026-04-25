#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

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
  ./scripts/release_ship.sh [--bump patch|minor|major] [--skip-dry-run] [--base main]
  ./scripts/release_ship.sh --finalize [--skip-dry-run] [--notes-output path] [--base main]

Merge-queue-safe release sequence for a prepared Harn release.

Assumptions:
  - Codex or a human has already reviewed pending tracked/untracked work.
  - README.md, CLAUDE.md, docs/, spec/, and CHANGELOG.md were updated as needed.
  - The intended release content has already landed on main through the merge queue.
  - The current worktree is clean before this script starts.

Default mode then:
  1. Runs ./scripts/release_gate.sh audit
  2. Optionally runs ./scripts/release_gate.sh publish --dry-run
  3. Creates release/vX.Y.Z
  4. Runs ./scripts/release_gate.sh prepare --bump ...
  5. Commits the version bump
  6. Pushes release/vX.Y.Z and opens a "Bump version to X.Y.Z" PR.

After that PR lands through the merge queue, run:

  ./scripts/release_ship.sh --finalize

Environment variables:
  HARN_BOOTSTRAP_NEW_CRATES=1
    First-release bootstrap mode for a brand-new workspace crate that
    an already-published crate now depends on. Skips the publish
    dry-run and tells verify_crate_packages.sh to skip the harn-cli
    package check (which fails when a path-dep crate isn't on
    crates.io yet). The real publish later uses
    `cargo publish --workspace`, which orders intra-workspace deps
    correctly. See harn#609.

Finalize mode:
  1. Runs ./scripts/release_gate.sh audit
  2. Optionally runs ./scripts/release_gate.sh publish --dry-run
  3. Creates/pushes tag vX.Y.Z from main
  4. Runs ./scripts/release_gate.sh publish to upload crates to crates.io
  5. Renders changelog-backed release notes
  6. Creates/updates a GitHub release with the rendered notes
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
  # env var via verify_crate_packages.sh and skips the harn-cli check.
  # See harn#609 for the full failure mode.
  if [[ "${HARN_BOOTSTRAP_NEW_CRATES:-0}" == "1" ]]; then
    echo "=== HARN_BOOTSTRAP_NEW_CRATES=1: skipping publish dry-run ==="
    SKIP_DRY_RUN=1
  fi

  log_step "Release audit"
  ./scripts/release_gate.sh audit

  if [[ "$SKIP_DRY_RUN" -eq 0 ]]; then
    log_step "Publish dry run"
    ./scripts/release_gate.sh publish --dry-run
  fi
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

  log_step "Version bump"
  ./scripts/release_gate.sh prepare --bump "$BUMP"
  local actual_next
  actual_next="$(current_version)"
  if [[ "$actual_next" != "$next" ]]; then
    echo "error: expected version $next, got $actual_next"
    exit 1
  fi

  log_step "Commit version bump"
  git add Cargo.toml Cargo.lock crates/*/Cargo.toml
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
    gh pr create \
      --base "$base" \
      --head "$branch" \
      --title "Bump version to $next" \
      --body-file "$body_file"
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
  echo "  Total wall time:  $(_ship_fmt_ns "$TOTAL_NS")"
  echo "  Finalize after merge queue lands it: ./scripts/release_ship.sh --finalize"
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
    git tag "$tag"
  fi
}

BUMP="patch"
SKIP_DRY_RUN=0
MODE="bump-pr"
BASE_BRANCH="main"
NOTES_OUTPUT=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --bump)
      BUMP="${2:-}"
      shift 2
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
    --notes-output)
      NOTES_OUTPUT="${2:-}"
      shift 2
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

require_clean_tree
require_base_branch "$BASE_BRANCH"

PREVIOUS_VERSION="$(current_version)"
if [[ -z "$PREVIOUS_VERSION" ]]; then
  echo "error: failed to detect current version"
  exit 1
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
./scripts/release_gate.sh publish

if [[ -z "$NOTES_OUTPUT" ]]; then
  NOTES_OUTPUT="$(mktemp)"
  CLEANUP_NOTES=1
else
  CLEANUP_NOTES=0
fi

log_step "Release notes"
./scripts/release_gate.sh notes --version "$TAG" --output "$NOTES_OUTPUT"
cat "$NOTES_OUTPUT"

# Create or update GitHub release with rendered notes as the LAST step, so
# the release body reflects the final crates.io + git state. If crates.io
# was slow, the upstream tag is already live — downstream CI will have
# kicked off minutes ago.
GH_RELEASE_URL=""
if command -v gh &>/dev/null; then
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
