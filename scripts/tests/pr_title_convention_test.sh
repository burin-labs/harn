#!/usr/bin/env bash
# Every pull request this repository opens for itself must satisfy the same
# title gate a person's pull request does.
#
# The release bot's post-tag development bump was titled "Start 0.10.129-dev
# development" and failed the required "PR title and description" check, which
# left the cutover pull request unmergeable until someone retitled it by hand.
# Nothing local caught it, because the gate is a shared action that only runs on
# a real pull request event, and the failure appears one release later than the
# change that caused it.
#
# This asserts the shape rather than re-implementing the gate: every literal
# title passed to `gh pr create` in this repository's scripts either carries a
# bracketed area, or is one of the two subjects the action exempts by name. The
# area list is read from the workflow that configures the action, so the two
# cannot drift apart.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
workflow="$root/.github/workflows/pr-gates.yml"

# The exact list handed to the action. Read, never restated: a copy here would
# pass while the gate rejected the same title.
areas_line="$(grep -E '^ +areas: "' "$workflow" || true)"
if [[ -z "$areas_line" ]]; then
  echo "pr_title_convention_test: could not read the area list from pr-gates.yml" >&2
  echo "  the gate's configuration moved; this check cannot verify anything without it" >&2
  exit 1
fi
areas="$(sed 's/.*areas: "\(.*\)".*/\1/' <<<"$areas_line")"
if [[ -z "$areas" || "$areas" != *"Release"* ]]; then
  echo "pr_title_convention_test: the area list read back empty or without Release" >&2
  exit 1
fi

# Subjects the action exempts by name. `Release vX.Y.Z` is matched verbatim by
# publish-release.yml before it tags, so it must not gain a bracket.
exempt_pattern='^Release v[0-9]'

status=0
found=0
while IFS= read -r hit; do
  file="${hit%%:*}"
  rest="${hit#*:}"
  line="${rest%%:*}"
  text="${rest#*:}"
  # The literal between --title " and the closing quote.
  title="$(sed 's/.*--title "\([^"]*\)".*/\1/' <<<"$text")"
  # Skip the shell's own echoed hints, which are documentation and not calls.
  if [[ "$text" == *'echo '* ]]; then
    continue
  fi
  found=$((found + 1))
  if [[ "$title" =~ $exempt_pattern ]]; then
    continue
  fi
  if [[ ! "$title" =~ ^\[($areas)\][[:space:]] ]]; then
    echo "pr_title_convention_test: $file:$line opens a pull request titled" >&2
    echo "  \"$title\"" >&2
    echo "  which the required PR title gate rejects. Prefix it with one area in" >&2
    echo "  square brackets, for example \"[Release] $title\"." >&2
    status=1
  fi
# This file is excluded: it quotes the pattern it searches for, so scanning it
# would match its own documentation rather than a pull-request title.
done < <(
  grep -rn --include='*.sh' -- '--title "' "$root/scripts" \
    | grep -v 'gh release create' \
    | grep -v 'scripts/tests/pr_title_convention_test.sh'
)

# A probe that matched nothing would pass while proving nothing.
if [[ "$found" -eq 0 ]]; then
  echo "pr_title_convention_test: found no pull-request titles to check" >&2
  echo "  the search stopped matching; it is not evidence that the titles are fine" >&2
  exit 1
fi

if [[ "$status" -ne 0 ]]; then
  exit 1
fi

echo "pr_title_convention_test: ok ($found title(s) checked against $areas)"
