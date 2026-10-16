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
    (.repository.full_name) as $repository
    | (.pull_request.base.repo.full_name) as $base_repository
    | (.pull_request.head.repo.full_name) as $head_repository
    | if all([$repository, $base_repository, $head_repository][];
        type == "string" and test("^[A-Za-z0-9][A-Za-z0-9-]*/[A-Za-z0-9][A-Za-z0-9_.-]*$"))
        and $repository == $base_repository
      then [.pull_request.base.sha, .pull_request.head.sha,
        (if $head_repository == $repository then "origin"
         else "https://github.com/" + $head_repository + ".git" end)]
      else error("invalid PR gate source repository identity") end
  elif (.merge_group | type) == "object" then
    [.merge_group.base_sha, .merge_group.head_sha, "origin"]
  else error("missing PR gate commit range") end
  | if all(.[0:2][]; type == "string" and test("^[0-9a-fA-F]{40,64}$"))
    then [(.[0] | ascii_downcase), (.[1] | ascii_downcase), .[2]] | @tsv
    else error("invalid PR gate commit range") end
' "$event_path")
IFS=$'\t' read -r base_sha head_sha head_remote <<<"$range"
if [[ $(git rev-parse --is-shallow-repository) != false ]]; then
  echo 'error: PR gate range requires complete ancestry, not a shallow checkout' >&2
  exit 1
fi
for endpoint in base head; do
  sha=$base_sha
  remote=origin
  if [[ "$endpoint" == head ]]; then
    sha=$head_sha
    remote=$head_remote
  fi
  if ! git cat-file -e "${sha}^{commit}" 2>/dev/null; then
    # No depth option: retain the original PR ancestry and merge-base.
    git fetch --no-tags "$remote" "$sha"
  fi
  if [[ $(git rev-parse --verify "${sha}^{commit}") != "$sha" ]] \
    || ! git cat-file -e "${sha}^{tree}"; then
    echo "error: PR gate commit or tree is unreadable: $sha" >&2
    exit 1
  fi
done
merge_base=$(git merge-base "$base_sha" "$head_sha")
echo "PR gate range: base=$base_sha head=$head_sha merge_base=$merge_base endpoints=2 pending=0"
