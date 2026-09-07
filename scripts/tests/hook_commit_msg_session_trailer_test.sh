#!/usr/bin/env bash
# Behavioral coverage for the commit-msg session-trailer strip.
#
# Each case runs the REAL hook against a real message file and asserts on the
# bytes it leaves behind, rather than re-implementing the pattern. The arm that
# matters most is the last one: a message with no trailer must come back byte
# for byte identical, because a hook that rewrites ordinary commit messages is
# worse than the leak it closes.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
hook="$repo_root/.githooks/commit-msg"

tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

passes=0
failures=0

run_hook() {
  # Returns the rewritten message on stdout. The hook's own notice goes to
  # stderr and is deliberately not captured into the comparison.
  local body=$1
  local file="$tmp_root/msg.$RANDOM.$RANDOM"
  printf '%s' "$body" > "$file"
  "$hook" "$file" 2>/dev/null
  cat "$file"
  rm -f "$file"
}

check() {
  local name=$1 actual=$2 expected=$3
  if [[ "$actual" == "$expected" ]]; then
    passes=$((passes + 1))
    printf 'ok - %s\n' "$name"
  else
    failures=$((failures + 1))
    printf 'FAIL - %s\n' "$name"
    printf '  expected: %q\n' "$expected"
    printf '  actual:   %q\n' "$actual"
  fi
}

# A trailer whose key ends in `Session` and whose value is a URL is removed,
# and the blank line the removal exposes goes with it.
check "strips a session URL trailer and the blank line it leaves" \
  "$(run_hook 'Subject line

Body paragraph.

Co-Authored-By: Someone <someone@example.com>
Claude-Session: https://example.invalid/code/session_abc
')" \
  'Subject line

Body paragraph.

Co-Authored-By: Someone <someone@example.com>'

# The key is matched by shape, not by name, so a tool this repository has never
# seen is covered the day it ships.
check "strips an unknown tool's session trailer" \
  "$(run_hook 'Subject line

SomeOtherToolSession: https://example.invalid/s/1
')" \
  'Subject line'

# The hyphenated and unhyphenated spellings are the same trailer.
check "strips the unhyphenated spelling" \
  "$(run_hook 'Subject line

AgentSession: https://example.invalid/s/2
')" \
  'Subject line'

# Several trailers in one message all go.
check "strips every session trailer in one message" \
  "$(run_hook 'Subject line

A-Session: https://example.invalid/1
Signed-off-by: Someone <someone@example.com>
B-Session: https://example.invalid/2
')" \
  'Subject line

Signed-off-by: Someone <someone@example.com>'

# The URL is what makes the line an exposure. Prose ending in "Session" is not
# a trailer and must survive.
check "leaves a session word that is not a URL trailer" \
  "$(run_hook 'Subject line

Session: the third one of the day.
Rework the Session: handling in the parser.
')" \
  'Subject line

Session: the third one of the day.
Rework the Session: handling in the parser.'

# A trailer indented, or mid-line, is not a trailer.
check "leaves an indented or mid-line session URL alone" \
  "$(run_hook 'Subject line

  Claude-Session: https://example.invalid/s/3
See Claude-Session: https://example.invalid/s/4 for context.
')" \
  'Subject line

  Claude-Session: https://example.invalid/s/3
See Claude-Session: https://example.invalid/s/4 for context.'

# The direction control. An ordinary message must come back unchanged, so a
# green run above cannot be explained by the hook rewriting everything.
ordinary='Subject line

Body paragraph explaining the change.

Co-Authored-By: Someone <someone@example.com>'
check "leaves an ordinary message byte for byte" \
  "$(run_hook "$ordinary")" \
  "$ordinary"

# A missing or absent argument must not abort a commit.
"$hook" >/dev/null 2>&1 && argless=0 || argless=$?
check "exits clean with no argument" "$argless" "0"
"$hook" "$tmp_root/does-not-exist" >/dev/null 2>&1 && missing=0 || missing=$?
check "exits clean on a missing file" "$missing" "0"

printf '\n%d passed, %d failed\n' "$passes" "$failures"
[[ "$failures" -eq 0 ]]
