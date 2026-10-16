#!/usr/bin/env bash
# Name the public API check's "before" commit and make its ancestry with HEAD
# readable in a shallow checkout.
#
# The baseline is a pull request merge commit's first parent, the merge
# group's base, or the pushed range's `before`. Only the first is always one
# commit behind HEAD. A merge group or push that lands several pull requests
# puts the baseline further back, beyond a depth-2 checkout's shallow
# boundary, so a depth-1 fetch of the baseline alone leaves two disconnected
# islands and `git merge-base` has nothing to answer. This deepens HEAD's
# history until the baseline is reachable, and refuses when it is not within
# the cap rather than letting the comparison read as measured.
#
# Inputs (environment):
#   EVENT                 github.event_name
#   MERGE_GROUP_BASE_SHA  github.event.merge_group.base_sha
#   PUSH_BEFORE_SHA       github.event.before
#   GITHUB_OUTPUT         receives base_sha=<sha> (optional outside Actions)
#   PUBLIC_API_BASELINE_MAX_DEEPEN  commits of history to add before refusing
#                         (default 512)
set -euo pipefail

TITLE="Public API baseline"
max_deepen=${PUBLIC_API_BASELINE_MAX_DEEPEN:-512}

case "${EVENT:-}" in
  pull_request) base=$(git rev-parse HEAD^1) ;;
  merge_group) base=${MERGE_GROUP_BASE_SHA:-} ;;
  *) base=${PUSH_BEFORE_SHA:-} ;;
esac
if [[ ! "$base" =~ ^[0-9a-fA-F]{40,64}$ ]] || [[ "$base" =~ ^0+$ ]]; then
  echo "::error title=$TITLE::event ${EVENT:-<unset>} names no baseline commit ('$base'); surface comparison is unmeasured." >&2
  exit 1
fi
head_sha=$(git rev-parse HEAD)

if ! git cat-file -e "${base}^{commit}" 2>/dev/null; then
  git fetch --no-tags --depth=1 origin "$base"
fi

deepened=0
step=8
until git merge-base "$base" "$head_sha" >/dev/null 2>&1; do
  if [[ $(git rev-parse --is-shallow-repository) != true ]]; then
    echo "::error title=$TITLE::$base shares no history with $head_sha; surface comparison is unmeasured." >&2
    exit 1
  fi
  if (( deepened >= max_deepen )); then
    echo "::error title=$TITLE::$base is not within $deepened commits of $head_sha; surface comparison is unmeasured." >&2
    exit 1
  fi
  git fetch --no-tags --deepen="$step" origin "$head_sha"
  deepened=$((deepened + step))
  step=$((step * 2))
done

merge_base=$(git merge-base "$base" "$head_sha")
echo "$TITLE: event=${EVENT:-} base=$base head=$head_sha merge_base=$merge_base deepened=$deepened"
if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
  echo "base_sha=$base" >> "$GITHUB_OUTPUT"
fi
