# Maintainer release workflow

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

`scripts/publish.sh` is the thin entrypoint for the Harn publisher used by the
release gate. Live publication probes each crate version, resumes the remaining
dependency DAG, and waits with bounded backoff before publishing dependents of
newly uploaded crates. It emits a JSON receipt separating published,
already-present, waiting, failed, and remaining crates. Dry-run mode continues
to use Cargo's workspace dry-run because it has no remote recovery state.

## Release artifacts

Every published release uploads five per-target archives, a
coreutils-format `SHA256SUMS` manifest, and a structured
`release-assets.json` manifest. Downstream packagers
(downstream `fetch-harn.sh` scripts, npm CLI postinstall hooks,
Scoop/Homebrew formula generators) should prefer the structured
manifest. See [Release assets manifest](./dev/release-assets-manifest.md)
for the schema and stable URLs.
