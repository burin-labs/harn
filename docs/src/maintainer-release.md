# Maintainer release workflow

This page is for Harn maintainers cutting a release. User-facing CLI behavior
lives in [CLI reference](./cli-reference.md).

## Standard flow

Live releases run from the protected hosted workflow owned by
`burin-labs/harn-bump-fleet`. Start from an up-to-date Harn checkout, freeze the
exact remote source SHA, and dispatch the Fleet workflow:

```bash
git fetch origin main
HARN_RELEASE_SHA="$(git rev-parse origin/main)"
gh workflow run hosted-release.yml \
  --repo burin-labs/harn-bump-fleet \
  -f bump=patch \
  -f mode=ship-pr \
  -f at_sha="${HARN_RELEASE_SHA}"
```

Approve the run's protected `release` environment, then follow the exact run
until it hands off the immutable tag and release PR. Once the tag is known,
resume Fleet's durable post-tag watcher from a `harn-bump-fleet` checkout:

```bash
scripts/watch_harn_release.sh \
  --tag vX.Y.Z \
  --repo ../harn \
  --yes-live-release
```

The hosted workflow owns source freezing, audits, hosted platform
certification, the GitHub-signed release commit, immutable tag, release PR, and
auto-merge. The watcher is resumable by exact receipt and owns missing-asset
recovery, PR re-arming, release finalization, main-cache warming, and transient
ref cleanup. A visible tag or prerelease is an intermediate state, not release
completion.

Do not invoke `scripts/release_ship.sh` or the local `release_harn.harn` harness
for a normal live release. They are implementation and development surfaces;
the hosted workflow is the authority boundary for release credentials,
signatures, and protected-environment approval. If a run stops after the tag is
published, rerun the watcher first: it reuses the immutable attempt and avoids
duplicating accepted builds or publication work.

Before cutting a release that adds a new hard preflight requirement, verify its
user-facing documentation includes an equivalent migration note: the exact
command for auditing data accepted by the prior release, a typed non-success
status that cannot be mistaken for compliance, the records requiring review,
and the exact command that returns the user to strict mode. A compatibility
path may support review, but it must not manufacture evidence or weaken the
final production/export gate.

## Hosted platform certification

Release preparation is fail-closed on the frozen remote source SHA. Before the
version/changelog commit is created, the release harness dispatches
`.github/workflows/windows-nightly.yml` and
`.github/workflows/macos-nightly.yml` for the frozen source branch while the
local source audit runs. GitHub must return an exact run ID for each dispatch.
Both runs and their full-workspace jobs must complete successfully with the
expected workflow path, event, SHA, URL, and unique job identity.

Windows certification stays off the contended Actions cache namespace.
Successful `main` `windows-nightly` runs publish a short-retention
`workspace-windows-warm` workflow artifact; `release-certify/<sha>` consumers
restore that artifact read-only into a larger Dynamic Dev Drive ceiling and
fall cold when no compatible generation exists. Cargo still owns exact-source
invalidation after the restore. Artifact name, retention, size budget, nextest
pin, and Dev Drive ceilings are owned by `.github/cache-policy.json`
(`windows_workspace_warm`) and locked by `scripts/check_ci_cache_policy.harn`.

The resulting `harn.release_audit_receipt.v2` records the certified source SHA,
run/job URLs and IDs, per-lane timings, and critical path. The harness re-reads
the remote branch after the join; movement invalidates the whole receipt. The
release harness runs the residual checks affected by release metadata, creates
the synthetic release commit, and then proves that commit has exactly the
certified SHA as its sole parent.

If a hosted run fails or is cancelled, fix the source or runner problem and
restart the release from the still-unmodified source branch. If a valid exact
run is already recorded, reuse its receipt; do not dispatch a blind duplicate.
If the branch moved, discard both platform receipts and freeze the new SHA.

### Diagnose a source audit failure

Open the failed hosted release artifact and read `release-audit.json`. Find the
`hosted-platform-certification` step. Its output ends with `RELEASE AUDIT
FAILURE RECAP`, the failed lane, its exit status, and the last 40 log lines.
Use that cause to choose the narrowest local check. Do not rerun a release only
because the workflow page shows a generic `harn-audit failed` message.

For a Harn conformance failure, rerun the named file with the frozen candidate
binary and the release network environment cleared:

```bash
env -u HARN_EGRESS_ALLOW \
  -u HARN_EGRESS_DENY \
  -u HARN_EGRESS_DEFAULT \
  -u HARN_EGRESS_BLOCK_PRIVATE \
  -u HARN_EGRESS_ALLOW_LOOPBACK \
  HARN_BIN=/path/to/frozen/harn \
  ./scripts/harn_bin.sh -- test conformance/tests/path/to/test.harn
```

If the focused test passes, replay the full conformance set more than once.
Treat one pass as evidence of a transient failure, not proof that the cause is
gone. Keep the failed receipt and the replay logs with the release record.

## Piecewise gates

Use the repository-local gates only when you need to audit or dry-run without
opening a release PR:

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
