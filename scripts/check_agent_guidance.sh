#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

fail() {
  echo "agent guidance check failed: $*" >&2
  exit 1
}

[[ -L CLAUDE.md ]] || fail "CLAUDE.md must be a symlink to AGENTS.md"
[[ "$(readlink CLAUDE.md)" == "AGENTS.md" ]] ||
  fail "CLAUDE.md must point directly to AGENTS.md"

grep -Fq "docs/src/dev/engineering-principles.md" AGENTS.md ||
  fail "AGENTS.md must link the engineering principles"
grep -Fqi "one owner" AGENTS.md ||
  fail "AGENTS.md must preserve the one-owner rule"
if grep -Fqi "simpler and dumber" AGENTS.md; then
  fail "AGENTS.md contains the retired outcome-shrinking phrase"
fi

for skill in harn-agent harn-de-slop harn-docs harn-orchestration harn-probe harn-product-quality harn-testing; do
  [[ -f "crates/harn-skills/src/corpus/$skill/SKILL.md" ]] ||
    fail "missing canonical skill $skill"
done

echo "Agent guidance is canonical and linked."
