---
name: release-harn
description: Alias for the Harn release workflow skill.
---

# Release Harn

Use the same workflow as [`harn-release`](../harn-release/SKILL.md).

Default live command:

```bash
cd ~/projects/harn-bump-fleet
harn run --no-sandbox release_harn.harn -- \
  --repo ~/projects/harn --mode ship-pr --agent --yes-live-release
```
