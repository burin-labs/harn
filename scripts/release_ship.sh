#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

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
  5. Renders changelog-backed release notes
  6. Runs ./scripts/release_gate.sh publish
  7. Creates tag vX.Y.Z
  8. Pushes the current branch and tag unless --no-push was passed
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

echo "=== Release audit ==="
./scripts/release_gate.sh audit

if [[ "$SKIP_DRY_RUN" -eq 0 ]]; then
  echo "=== Publish dry run ==="
  ./scripts/release_gate.sh publish --dry-run
fi

echo "=== Version bump ==="
./scripts/release_gate.sh prepare --bump "$BUMP"
NEXT_VERSION="$(current_version)"

if [[ "$NEXT_VERSION" == "$PREVIOUS_VERSION" ]]; then
  echo "error: version did not change"
  exit 1
fi

git add Cargo.toml Cargo.lock
git commit -m "Bump version to $NEXT_VERSION"

TAG="v$NEXT_VERSION"
BRANCH="$(git branch --show-current)"

if [[ -z "$NOTES_OUTPUT" ]]; then
  NOTES_OUTPUT="$(mktemp)"
  CLEANUP_NOTES=1
else
  CLEANUP_NOTES=0
fi

echo "=== Release notes ==="
./scripts/release_gate.sh notes --version "$TAG" --output "$NOTES_OUTPUT"
cat "$NOTES_OUTPUT"

echo "=== Publish ==="
./scripts/release_gate.sh publish

echo "=== Tag ==="
git tag "$TAG"

if [[ "$NO_PUSH" -eq 0 ]]; then
  echo "=== Push ==="
  git push origin "$BRANCH"
  git push origin "$TAG"
fi

echo ""
echo "Release shipped:"
echo "  Previous version: $PREVIOUS_VERSION"
echo "  Current version:  $NEXT_VERSION"
echo "  Branch:           $BRANCH"
echo "  Tag:              $TAG"
echo "  Notes file:       $NOTES_OUTPUT"
if [[ "$NO_PUSH" -eq 1 ]]; then
  echo "  Push status:      skipped (--no-push)"
else
  echo "  Push status:      pushed branch and tag"
fi

if [[ "$CLEANUP_NOTES" -eq 1 ]]; then
  rm -f "$NOTES_OUTPUT"
fi
