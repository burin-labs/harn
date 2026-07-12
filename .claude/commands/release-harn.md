Run the tag-first Harn release workflow.

The canonical playbook is
`.codex/skills/harn-release/SKILL.md`; use it with the repo scripts as the
source of truth.

Default live command:

```bash
cd ~/projects/harn-bump-fleet
harn run --no-sandbox release_harn.harn -- \
  --repo ~/projects/harn --mode ship-pr --agent --yes-live-release
```

`ship-pr` prepares the release content, commits it, pushes the branch, pushes
the signed `vX.Y.Z` tag at the pinned release commit, opens the `Release
vX.Y.Z` PR, and enables auto-merge. The tag is pushed before the PR merges so
publishing is tied to the pinned tag commit.

Do not run `scripts/release_ship.sh --prepare` directly for normal releases.
It is an implementation detail of `release_harn.harn` and refuses standalone
use. Use `scripts/release_ship.sh --finalize`, `scripts/release_ship.sh --bump`,
and the release workflows only for recovery after reading their help text.
