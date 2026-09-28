---
name: release-harn
short: Cut and verify a Harn release through its owning workflows.
description: Follow Harn's release PR, exact candidate build, promotion, and downstream repin procedure.
when_to_use: Use when cutting a Harn patch release from main, or recovering a partial release.
---

# Release Harn

Follow the [maintainer release how-to](https://github.com/burin-labs/harn/blob/main/docs/src/maintainer-release.md).
It owns the commands, release authority, recovery, and terminal proof. The
local discovery aliases point here; do not copy the procedure into them.

Harn's release opener prepares the PR. The merge queue builds and certifies
the exact commit that will land. Promotion verifies the candidate manifest and
publishes those same files before creating the tag. Generated fleet repin owns
downstream updates; publication and downstream convergence are separate claims.

Do not create a tag by hand, revive `hosted-release.yml`, or invoke
`release_ship.sh` as a parallel normal publisher. Preserve an explicitly frozen
candidate and follow the current owner's merge authority. Read the actual
workflow inputs before recovering an attempt; retired candidate inputs are
not accepted by the build workflow.
