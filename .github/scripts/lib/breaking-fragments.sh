# shellcheck shell=bash
#
# The one owner of "this pull request declares a breaking change".
#
# A breaking change is declared by a `changelog.d/<id>.breaking.md` fragment
# that says what a downstream consumer changes: a `Migration:` line followed
# by the change. Downstream consumers read the folded Breaking section to
# learn what to change, so a fragment without its migration declares nothing.
#
# Sourced by:
#   .github/scripts/changelog-fragment-check.sh  (fragment shape, `breaking` label)
#   .github/scripts/breaking-surface-check.sh    (CLI surface and Rust API breaks)

# Read a fragment body on stdin; succeed when it has a `Migration:` line with
# something after it, on the same line or below.
breaking_fragment_has_migration() {
  awk '
    found { if ($0 ~ /[^[:space:]]/) { body = 1 } next }
    /^[[:space:]]*(-[[:space:]]+)?(\*\*)?Migration:(\*\*)?/ {
      found = 1
      rest = $0
      sub(/^[[:space:]]*(-[[:space:]]+)?(\*\*)?Migration:(\*\*)?/, "", rest)
      if (rest ~ /[^[:space:]]/) { body = 1 }
    }
    END { exit body ? 0 : 1 }
  '
}

# declared_breaking_fragments MERGE_BASE HEAD
#
# Print, one per line, every breaking fragment this range adds or edits that
# carries its migration. A breaking fragment in the range without one is a
# malformed declaration: report it as a GitHub error annotation titled
# "$BREAKING_FRAGMENT_GATE_TITLE" and return 1. A fragment the range deletes
# has nothing to check.
declared_breaking_fragments() {
  local merge_base=$1 head=$2 fragment body
  local title="${BREAKING_FRAGMENT_GATE_TITLE:-Breaking change}"
  local fragments
  fragments=$(git diff --name-only --no-renames "$merge_base" "$head" -- changelog.d \
    | grep -E '^changelog\.d/[A-Za-z0-9_-]+\.breaking\.md$' || true)
  for fragment in $fragments; do
    if ! body=$(git show "$head:$fragment" 2>/dev/null); then
      continue
    fi
    if ! printf '%s\n' "$body" | breaking_fragment_has_migration; then
      echo "::error title=$title::$fragment is a breaking change with no \`Migration:\` section. Say what a downstream consumer changes, preferably as a before-and-after snippet." >&2
      return 1
    fi
    printf '%s\n' "$fragment"
  done
}
