#!/usr/bin/env bash
# Regression test for release_ship.sh's require_no_unfolded_fragments guardrail.
#
# Context: release_ship.sh does NOT fold changelog.d/*.<category>.md fragments
# (the fold lives in the bump-fleet release_harn 'prepare' flow). Invoking
# release_ship directly with fragments still present would ship a release whose
# CHANGELOG omits them and whose --finalize renders empty notes. The guardrail
# runs first inside run_common_gates and must fail loud. This dogfoods the exact
# failure mode hit during the v0.9.21 cut.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
ship_script="$repo_root/scripts/release_ship.sh"

# Extract just the guardrail function so we can exercise it in isolation without
# running the whole release (which builds crates). If the function is renamed or
# removed this extraction yields nothing and the first assertion fails loudly.
guard_src=$(sed -n '/^require_no_unfolded_fragments() {/,/^}/p' "$ship_script")
if [[ -z "$guard_src" ]]; then
  echo "FAIL: require_no_unfolded_fragments not found in $ship_script" >&2
  exit 1
fi

tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

run_guard() {
  # Run the guardrail in a subshell cd'd into $1, capture exit + stderr.
  local workdir="$1"
  ( cd "$workdir" && eval "$guard_src"'; require_no_unfolded_fragments' )
}

# --- Case 1: no fragments (only non-fragment files) -> PASS (exit 0) ----------
clean="$tmp_root/clean"
mkdir -p "$clean/changelog.d"
touch "$clean/changelog.d/.gitkeep" \
      "$clean/changelog.d/.markdownlint-cli2.jsonc" \
      "$clean/changelog.d/README.md"
if err=$(run_guard "$clean" 2>&1); then
  :
else
  echo "FAIL: guardrail rejected a fragment-free changelog.d" >&2
  echo "$err" >&2
  exit 1
fi

# --- Case 2: one unfolded fragment -> FAIL (exit 1) with a listing ------------
one="$tmp_root/one"
mkdir -p "$one/changelog.d"
printf -- '- scoped-write fix\n' > "$one/changelog.d/4199.fixed.md"
if err=$(run_guard "$one" 2>&1); then
  echo "FAIL: guardrail did not catch an unfolded fragment" >&2
  exit 1
fi
grep -q "1 unfolded changelog fragment" <<<"$err" \
  || { echo "FAIL: missing count in message: $err" >&2; exit 1; }
grep -q "4199.fixed.md" <<<"$err" \
  || { echo "FAIL: message did not list the fragment: $err" >&2; exit 1; }
grep -q "release_harn.harn" <<<"$err" \
  || { echo "FAIL: message did not point at the fold remediation: $err" >&2; exit 1; }

# --- Case 3: fragments across multiple categories -> FAIL, correct count ------
multi="$tmp_root/multi"
mkdir -p "$multi/changelog.d"
printf -- '- a\n' > "$multi/changelog.d/3872.added.md"
printf -- '- b\n' > "$multi/changelog.d/foo.changed.md"
printf 'bare paragraph\n' > "$multi/changelog.d/bar.breaking.md"
if err=$(run_guard "$multi" 2>&1); then
  echo "FAIL: guardrail did not catch multiple fragments" >&2
  exit 1
fi
grep -q "3 unfolded changelog fragment" <<<"$err" \
  || { echo "FAIL: wrong count for 3 fragments: $err" >&2; exit 1; }

# --- Case 4: no changelog.d dir at all -> PASS (exit 0) -----------------------
nodir="$tmp_root/nodir"
mkdir -p "$nodir"
if ! run_guard "$nodir" >/dev/null 2>&1; then
  echo "FAIL: guardrail errored when changelog.d/ is absent" >&2
  exit 1
fi

echo "release_ship_fragment_guard_test: ok"
