---
name: release-harn
short: Cut and verify an immutable Harn release through the owning harness.
description: Use harn-bump-fleet to prepare, certify, publish, and recover one exact Harn release candidate.
when_to_use: Use when cutting a stable, development, minor, or major Harn release from main, or recovering a partial release.
---

# Release Harn

`burin-labs/harn-bump-fleet` owns release preparation, candidate certification,
immutable attempt refs, publication, and recovery. Follow its
[release how-to](https://github.com/burin-labs/harn-bump-fleet/blob/main/docs/how-to/release-harn.md)
for the complete procedure and receipt contract. Keep that procedure in its
owning repository.

## Cut an exact candidate

Run from the `harn-bump-fleet` checkout:

```bash
scripts/with_env.sh harn run --no-sandbox release_harn.harn -- \
  --repo /path/to/harn --mode ship-pr --agent --yes-live-release \
  --at-sha <exact-main-commit> --expect-pr <required-pr>
```

- Select an exact main commit and require each release-critical PR with
  `--expect-pr`. The harness isolates the source checkout and preserves that
  pin throughout preparation.
- The harness owns version selection. A declared `X.Y.Z-dev` names the stable
  patch target; a stable workspace version uses the next version after the
  published floor. Do not implement a second version calculation.
- The harness prepares the `Release vX.Y.Z` PR and durable watch receipt.
  Follow the current merge authority before landing it.
- Never push to a PR after auto-merge is armed or while it is queued. Do not
  rebase an explicitly frozen candidate to absorb later main changes.
- Use the owning harness for recovery. Do not invoke `release_ship.sh`
  directly, create a tag by hand, or reconstruct a retired hosted launcher.

## Prove publication

Resume the durable watcher from the same `harn-bump-fleet` checkout:

```bash
scripts/watch_harn_release.sh --tag vX.Y.Z --repo /path/to/harn --yes-live-release
```

Completion requires the release PR to land, the signed tag and published
version to agree, the complete required asset manifest, and transient-ref
cleanup. The published files must be the certified files. Cache warming is
explicit; a `not_requested` receipt proves no warm was requested.

Downstream updates use the generated fleet bump orchestration and its
receipts. Their success is a separate claim from publication.

For cross-repository development, use the consumer's owning source-pin and
repin procedure. Release batching does not require waiting to test an
unreleased Harn commit.
