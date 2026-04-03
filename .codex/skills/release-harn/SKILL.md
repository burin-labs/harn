---
name: release-harn
description: Alias for the Harn release workflow skill. Use this when asked to prepare, audit, publish, tag, or document a Harn release.
---

# Release Harn

Use the same workflow as [`harn-release`](../harn-release/SKILL.md).

The repo source of truth remains:

```bash
./scripts/dev_setup.sh
./scripts/release_gate.sh <audit|prepare|publish|notes|full> ...
./scripts/release_ship.sh --bump patch
```

Before releasing, make sure the local developer workflow and observability
surface are documented coherently:

- `README.md`
- `CONTRIBUTING.md`
- `docs/src/portal.md`
- `scripts/dev_setup.sh`
- `Makefile`
- `.githooks/`
