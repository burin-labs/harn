---
name: release-harn
description: Alias for the Harn release workflow skill.
---

# Release Harn

Use the same workflow as [`harn-release`](../harn-release/SKILL.md).

Default live command:

```bash
cd ~/projects/harn-bump-fleet
scripts/with_env.sh harn run --no-sandbox release_harn.harn -- \
  --repo ~/projects/harn \
  --mode ship-pr \
  --at-sha <exact-origin-main-sha> \
  --expect-pr <required-pr-number> \
  --agent \
  --yes-live-release
```
