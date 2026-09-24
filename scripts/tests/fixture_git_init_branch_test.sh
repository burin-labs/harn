#!/usr/bin/env bash
# Every git repository a script test creates names its branch. Without `-b`, a
# repository's first branch, and a bare repository's HEAD, follow the host's
# init.defaultBranch: `main` on one machine and `master` on a CI runner. A
# fixture that clones or names `main` then passes on one host and fails on the
# other.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
self="scripts/tests/fixture_git_init_branch_test.sh"

inits="$(cd "$root" && git grep -n -E '(^|[^a-z])git( -C [^ ]+)? init( |$)' -- 'scripts/tests/*.sh' ':!'"$self" || true)"
if [[ -z "$inits" ]]; then
  echo "fixture_git_init_branch_test: found no git init in scripts/tests; the search stopped matching" >&2
  exit 1
fi
unnamed="$(grep -v -E -- '(-b |--initial-branch)' <<<"$inits" || true)"
if [[ -n "$unnamed" ]]; then
  echo "fixture_git_init_branch_test: these fixtures leave the branch to the host's init.defaultBranch; add -b main:" >&2
  echo "$unnamed" >&2
  exit 1
fi
echo "fixture_git_init_branch_test: ok ($(wc -l <<<"$inits" | tr -d ' ') git init calls name their branch)"
