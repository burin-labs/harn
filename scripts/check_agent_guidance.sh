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

release_skill=crates/harn-skills/src/corpus/release-harn/SKILL.md
[[ -f "$release_skill" ]] || fail "missing canonical release skill"
grep -Fq 'docs/src/maintainer-release.md' "$release_skill" ||
  fail "release skill must route to the maintainer procedure"
for source in AGENTS.md .codex/skills/harn-release/SKILL.md .codex/skills/release-harn/SKILL.md; do
  grep -Fq 'harn skill get release-harn --full' "$source" ||
    fail "$source must discover the embedded release skill"
done
for source in AGENTS.md .codex/skills/harn-release/SKILL.md .codex/skills/release-harn/SKILL.md "$release_skill" docs/src/maintainer-release.md; do
  if grep -Eq 'hosted-release\.yml|watch_harn_release|run_harn_release|release_harn\.harn|workflow run publish-release' "$source"; then
    fail "$source contains a retired or competing release entry point"
  fi
done

echo "Agent guidance is canonical and linked."
