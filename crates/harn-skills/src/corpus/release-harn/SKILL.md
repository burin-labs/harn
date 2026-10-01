---
name: release-harn
short: Release Harn through its owning GitHub workflows.
description: Open, certify, publish, and recover Harn releases through the maintainer release procedure.
when_to_use: Use when preparing or publishing a Harn release, checking its terminal evidence, or recovering a failed release run.
---

# Release Harn

Read the [maintainer release procedure](https://harnlang.com/docs/maintainer-release.html)
before release work. In a Harn checkout, read `docs/src/maintainer-release.md`
for the version-matched procedure.

That page owns commands, admission, frozen candidates, recovery, and terminal
proof. Follow its `bump-release.yml` opener, merge queue, candidate build,
promotion, and tag publication chain. `release_ship` is an implementation
surface, not a separate live release entry point.

Keep publication evidence separate from downstream consumer convergence.
Don't copy release commands into another skill or start a retired Fleet
launcher, local publisher, or watcher beside the owning workflows.
