---
name: release-harn
description: Alias for the Harn release workflow skill. Use this when asked to prepare, audit, publish, tag, or document a Harn release.
---

# Release Harn

Use the same workflow as [`harn-release`](../harn-release/SKILL.md).

The repo source of truth remains:

```bash
./scripts/dev_setup.sh
./scripts/release_ship.sh --bump patch                     # full release
./scripts/release_gate.sh <audit|prepare|publish|notes|full> ...  # piecewise
```

`release_ship.sh` runs audit → dry-run publish → bump → commit → tag →
push branch + tag → `cargo publish` → render notes → create GitHub
release. The push happens before `cargo publish` so downstream
consumers (e.g. `burin-code`'s `fetch-harn`, GitHub release-binary
workflows) can start working in parallel with crates.io.

Before releasing, make sure the local developer workflow and observability
surface are documented coherently:

- `README.md`
- `CONTRIBUTING.md`
- `docs/src/portal.md`
- `scripts/dev_setup.sh`
- `Makefile`
- `.githooks/`

Commit pattern for a real release:

1. `Prepare vX.Y.Z release` — code + docs + `CHANGELOG.md`, no Cargo.toml.
2. `Bump version to X.Y.Z` — created automatically by `release_ship.sh`.
