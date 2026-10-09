#!/usr/bin/env bash
# A full main checkout does not include a squash-merged PR's original head.
# Materialize the exact event endpoints before any diff or metadata gate reads them.
set -euo pipefail

event_path=${1:-${GITHUB_EVENT_PATH:-}}
if [[ -z "$event_path" || ! -f "$event_path" ]]; then
  echo 'error: PR gate range requires an event JSON file' >&2
  exit 1
fi
range=$(jq -er '
  if (.pull_request | type) == "object" then
    [.pull_request.base.sha, .pull_request.head.sha]
  elif (.merge_group | type) == "object" then
    [.merge_group.base_sha, .merge_group.head_sha]
  else error("missing PR gate commit range") end
  | if all(.[]; type == "string" and test("^[0-9a-fA-F]{40,64}$"))
    then map(ascii_downcase) | @tsv else error("invalid PR gate commit range") end
' "$event_path")
IFS=$'\t' read -r base_sha head_sha <<<"$range"
if [[ $(git rev-parse --is-shallow-repository) != false ]]; then
  echo 'error: PR gate range requires complete ancestry, not a shallow checkout' >&2
  exit 1
fi
for sha in "$base_sha" "$head_sha"; do
  if ! git cat-file -e "${sha}^{commit}" 2>/dev/null; then
    # No depth option: retain the original PR ancestry and merge-base.
    git fetch --no-tags origin "$sha"
  fi
  if [[ $(git rev-parse --verify "${sha}^{commit}") != "$sha" ]] \
    || ! git cat-file -e "${sha}^{tree}"; then
    echo "error: PR gate commit or tree is unreadable: $sha" >&2
    exit 1
  fi
done
merge_base=$(git merge-base "$base_sha" "$head_sha")
echo "PR gate range: base=$base_sha head=$head_sha merge_base=$merge_base endpoints=2 pending=0"
