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
  ./scripts/release_ship.sh [--bump patch|minor|major] [--skip-dry-run] [--notes-output path] [--no-push]

Deterministic release sequence for a prepared Harn release.

Assumptions:
  - Codex or a human has already reviewed pending tracked/untracked work.
  - README.md, CLAUDE.md, docs/, spec/, and CHANGELOG.md were updated as needed.
  - The intended release content has already been committed.
  - The current worktree is clean before this script starts.

This script then:
  1. Runs ./scripts/release_gate.sh audit
  2. Optionally runs ./scripts/release_gate.sh publish --dry-run
  3. Runs ./scripts/release_gate.sh prepare --bump ...
  4. Commits the version bump
  5. Creates tag vX.Y.Z
  6. Pushes the current branch and tag first (unless --no-push) so GitHub
     release-binary workflows and downstream fetchers (e.g. burin-code's
     fetch-harn) can start working in parallel with crates.io publication.
  7. Runs ./scripts/release_gate.sh publish to upload crates to crates.io.
  8. Renders changelog-backed release notes.
  9. Creates/updates a GitHub release with the rendered notes (requires gh
     CLI) as the final step, so the release body reflects crates.io state.
EOF
}

require_clean_tree() {
  if ! git diff --quiet --ignore-submodules HEAD --; then
    echo "error: working tree is dirty"
    echo "hint: commit README/docs/spec/changelog/release-content changes before running release_ship.sh"
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

BUMP="patch"
SKIP_DRY_RUN=0
NO_PUSH=0
NOTES_OUTPUT=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --bump)
      BUMP="${2:-}"
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
      NO_PUSH=1
      shift
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

PREVIOUS_VERSION="$(current_version)"
if [[ -z "$PREVIOUS_VERSION" ]]; then
  echo "error: failed to detect current version"
  exit 1
fi

log_step "Release audit"
./scripts/release_gate.sh audit

if [[ "$SKIP_DRY_RUN" -eq 0 ]]; then
  log_step "Publish dry run"
  ./scripts/release_gate.sh publish --dry-run
fi

log_step "Version bump"
./scripts/release_gate.sh prepare --bump "$BUMP"
NEXT_VERSION="$(current_version)"

if [[ "$NEXT_VERSION" == "$PREVIOUS_VERSION" ]]; then
  echo "error: version did not change"
  exit 1
fi

log_step "Commit version bump"
git add Cargo.toml Cargo.lock crates/*/Cargo.toml
git commit -m "Bump version to $NEXT_VERSION"

TAG="v$NEXT_VERSION"
BRANCH="$(git branch --show-current)"

log_step "Tag"
git tag "$TAG"

# Push branch + tag before cargo publish so downstream consumers (e.g.
# burin-code's fetch-harn script, GitHub release-binary workflows) can start
# working in parallel with crates.io publication. crates.io is slower than
# GitHub, and this ordering overlaps the two latencies.
if [[ "$NO_PUSH" -eq 0 ]]; then
  log_step "Push branch + tag"
  git push origin "$BRANCH"
  git push origin "$TAG"
fi

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
if [[ "$NO_PUSH" -eq 0 ]] && command -v gh &>/dev/null; then
  log_step "GitHub release"
  if gh release view "$TAG" &>/dev/null; then
    GH_RELEASE_URL="$(gh release edit "$TAG" --notes-file "$NOTES_OUTPUT" 2>&1)"
    echo "Updated existing release: $GH_RELEASE_URL"
  else
    GH_RELEASE_URL="$(gh release create "$TAG" --title "$TAG" --notes-file "$NOTES_OUTPUT" 2>&1)"
    echo "Created release: $GH_RELEASE_URL"
  fi
elif [[ "$NO_PUSH" -eq 0 ]]; then
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
if [[ "$NO_PUSH" -eq 1 ]]; then
  echo "  Push status:      skipped (--no-push)"
else
  echo "  Push status:      pushed branch and tag"
fi
if [[ -n "$GH_RELEASE_URL" ]]; then
  echo "  GitHub release:   $GH_RELEASE_URL"
fi

if [[ "$CLEANUP_NOTES" -eq 1 ]]; then
  rm -f "$NOTES_OUTPUT"
fi
