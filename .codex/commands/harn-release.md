# Harn release command

Run the tag-first Harn release workflow.

Use the canonical skill:
[`../skills/harn-release/SKILL.md`](../skills/harn-release/SKILL.md).

Default live command:

```bash
cd ~/projects/harn-bump-fleet
harn run --no-sandbox release_harn.harn -- \
  --repo ~/projects/harn --mode ship-pr --agent --yes-live-release
```

Do not run `scripts/release_ship.sh --prepare` directly for normal releases.
`scripts/open_release_pr.sh` (the "Open release PR" workflow) runs it to build
the `Release vX.Y.Z` pull request. Recovery helpers are listed in the skill and
in `scripts/release_ship.sh --help`.
