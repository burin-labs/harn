# Maintainer Release Workflow

This page is for Harn maintainers cutting a release. User-facing CLI behavior
lives in [CLI reference](./cli-reference.md).

## Standard flow

Once release content lands on `main` through the merge queue, open the
automated version-bump PR:

```bash
./scripts/release_ship.sh --bump patch
```

After that PR lands through the merge queue, finalize from an up-to-date
`main`:

```bash
./scripts/release_ship.sh --finalize
```

The bump command runs audit, dry-run publish, version bump, commit, push to
`release/vX.Y.Z`, and PR creation. Finalize runs audit, dry-run publish, tag
creation, tag push, crate publishing, and GitHub release creation.

The tag is pushed before crate publishing so release-binary workflows and other
downstream automation can start in parallel with crates.io publication.

## Piecewise gates

Use the lower-level gates when you need to audit or dry-run without opening a
release PR:

```bash
./scripts/release_gate.sh audit
./scripts/release_gate.sh full --bump patch --dry-run
```

`scripts/publish.sh` remains the crates.io publisher used by the release gate.
