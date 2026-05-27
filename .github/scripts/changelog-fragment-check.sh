#!/usr/bin/env bash
#
# Changelog fragment gate: PRs that change user-visible code must drop a
# `changelog.d/<id>.<category>.md` fragment OR edit `CHANGELOG.md`
# directly, or carry the `no-changelog-needed` label. Mirrors the
# `.github/scripts/demo-gate.sh` pattern.
#
# Inputs (from caller):
#   BASE_SHA          merge-base or PR base commit (default: origin/main)
#   HEAD_SHA          PR head commit (default: HEAD)
#   BYPASS_REASON     non-empty string makes the gate pass with a notice
#   GATE_ENABLE_TRACE non-empty enables verbose diagnostics on stderr
#
# Exit codes:
#   0 — fragment / changelog present, bypassed, or only ignored paths touched.
#   1 — user-visible change without an accompanying fragment or CHANGELOG edit.
#   2 — usage error.

set -euo pipefail

BASE_SHA="${BASE_SHA:-origin/main}"
HEAD_SHA="${HEAD_SHA:-HEAD}"
BYPASS_REASON="${BYPASS_REASON:-}"
GATE_ENABLE_TRACE="${GATE_ENABLE_TRACE:-}"

if [ -n "$BYPASS_REASON" ]; then
  echo "::notice title=Changelog fragment gate bypassed::$BYPASS_REASON"
  exit 0
fi

if ! merge_base=$(git merge-base "$BASE_SHA" "$HEAD_SHA" 2>/dev/null); then
  merge_base="$BASE_SHA"
fi
[ -n "$GATE_ENABLE_TRACE" ] && echo "[changelog-gate] base=$BASE_SHA head=$HEAD_SHA merge_base=$merge_base" >&2

changed_files=$(git diff --name-only --no-renames "$merge_base" "$HEAD_SHA")
if [ -z "$changed_files" ]; then
  echo "::notice title=Changelog fragment gate::no file changes; pass."
  exit 0
fi

# A direct CHANGELOG.md edit (operator authored straight into Unreleased)
# satisfies the gate. This is the existing path and intentionally remains.
if printf '%s\n' "$changed_files" | grep -qxF "CHANGELOG.md"; then
  echo "::notice title=Changelog fragment gate::CHANGELOG.md edited directly; pass."
  exit 0
fi

# Any new fragment under changelog.d/ satisfies the gate. We accept new
# fragments OR modifications to existing ones (uncommon but valid when a
# follow-up commit on the same PR refines the wording).
fragment_hits=$(printf '%s\n' "$changed_files" \
  | grep -E '^changelog\.d/[A-Za-z0-9_-]+\.(breaking|added|changed|deprecated|removed|fixed|security)\.md$' \
  || true)
if [ -n "$fragment_hits" ]; then
  count=$(printf '%s\n' "$fragment_hits" | wc -l | tr -d ' ')
  echo "::notice title=Changelog fragment gate::found $count changelog.d fragment(s); pass."
  exit 0
fi

# Determine whether the PR has any "user-visible" surface. The heuristic:
# any change outside the ignorable-by-default paths counts as user-visible
# and requires a fragment. This deliberately defaults strict so we don't
# silently ship runtime changes without notes.
ignored_pattern='^(\.github/|docs/|spec/|README(\.md)?$|AGENTS\.md$|CLAUDE\.md$|\.gitignore$|\.gitattributes$|\.editorconfig$|\.markdownlint\.yaml$|\.markdownlint-cli2\.yaml$|changelog\.d/(\.gitkeep|README\.md)$|tests?/|conformance/|benchmarks/|evals/|examples/|experiments/|test_fixtures/|perf/|playground/|tree-sitter-harn/|editors/)'
nontrivial=$(printf '%s\n' "$changed_files" | grep -Ev "$ignored_pattern" || true)
if [ -z "$nontrivial" ]; then
  echo "::notice title=Changelog fragment gate::only docs/test/CI paths touched; pass."
  exit 0
fi

# Failed gate. Print actionable guidance and the first few files that
# pushed the gate over the line.
{
  echo "::error title=Changelog fragment gate::This PR changes user-visible code but has no \`changelog.d/\` fragment."
  echo ""
  echo "To fix this, EITHER:"
  echo "  1. Add a fragment file: \`changelog.d/<pr-or-issue-num>.<category>.md\`"
  echo "     where <category> is one of: breaking, added, changed, deprecated, removed, fixed, security."
  echo "     See \`changelog.d/README.md\` for the format."
  echo ""
  echo "  2. Edit \`CHANGELOG.md\` directly under \`## Unreleased\` (legacy path; still accepted)."
  echo ""
  echo "  3. Apply the \`no-changelog-needed\` label if this PR genuinely needs no entry"
  echo "     (typo fixes, internal refactors, dep bumps with no user-visible effect)."
  echo ""
  echo "Triggering file(s) (up to 10):"
  printf '%s\n' "$nontrivial" | head -10 | sed 's/^/  - /'
} >&2
exit 1
