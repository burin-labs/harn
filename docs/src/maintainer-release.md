# Maintainer release workflow

This page is for Harn maintainers cutting a release. User-facing CLI behavior
lives in [CLI reference](./cli-reference.md).

## Standard flow

Harn owns release preparation, candidate certification, and promotion through
its repository workflows. Confirm the required changes have landed on main,
then ask the current release owner to dispatch the same opener used by the
daily schedule:

```bash
gh workflow run bump-release.yml --repo burin-labs/harn --ref main
```

The [opener](https://github.com/burin-labs/harn/blob/main/.github/workflows/bump-release.yml) has no dispatch inputs.
It reads main's development version and pending changelog fragments, prepares
the `Release vX.Y.Z` PR, and arms auto-merge. A stable version or no pending
fragments produces a measured no-op. Inspect its decision before retrying.
Follow the current release owner's merge authority. Never push to an armed or
queued PR, or refold an explicitly frozen candidate to absorb later changes.

The [candidate build](https://github.com/burin-labs/harn/blob/main/.github/workflows/build-release-binaries.yml)
builds, signs, notarizes, packages, attests, and checks the five-target matrix
at the release's exact merge queue commit. A pull-request run builds no release
files; its successful verdict is not archive proof. The main push reuses a
successful queue candidate at that same commit, or builds it when none exists.

[Promotion](https://github.com/burin-labs/harn/blob/main/.github/workflows/promote-release.yml) finds the successful
candidate run for that main commit. It verifies the manifest, file hashes, and
attestations, then publishes those same files and creates the tag. Nothing in
promotion rebuilds a release file. The tag starts crate publication; promotion
also starts the registered fleet repin, container packaging, and next
development-version PR.

Do not create tags by hand or invoke `scripts/release_ship.sh`, the local
`release_harn.harn` harness, or the retired Fleet `hosted-release.yml` launcher
as a parallel normal publisher. Read the current workflow inputs before a
recovery dispatch. The build workflow accepts warm-cache and benchmark inputs;
the retired `candidate_only`, `source_ref`, and `source_sha` inputs are not a
release entry point.

## Verify publication and recover

Record the release PR, its landed commit, successful candidate run, and
promotion run. Check that the tag resolves to the certified commit and that
the published files agree with `candidate-manifest.json`, including every
required archive and its attestation. Missing files, pending jobs, skipped
proof, and cancelled runs are not success.

Check crate publication and container packaging separately. Read the generated
fleet repin receipts to identify converged, failed, and still-pending consumers.
A published tag alone proves neither complete publication nor downstream
convergence.

Recover at the workflow that failed. Fix a source defect before rebuilding;
reuse a successful candidate at the exact commit for promotion recovery. Read
the failed job and its retained artifact before retrying, and retry only the
failed work once its prerequisite is available. An existing tag at a different
commit is a conflict for the release owner, not permission to replace the tag.

## Document a new preflight requirement

Before cutting a release that adds a new hard preflight requirement, verify its
user-facing documentation includes an equivalent migration note: the exact
command for auditing data accepted by the prior release, a typed non-success
status that cannot be mistaken for compliance, the records requiring review,
and the exact command that returns the user to strict mode. A compatibility
path may support review, but it must not manufacture evidence or weaken the
final production/export gate.

## Rehearse release changes offline

Before tagging, run the fixture rehearsal with an installed Harn binary:

```bash
HARN_BIN="$(command -v harn)" bash scripts/release_rehearsal.sh
```

The rehearsal executes the same staging script used by publication jobs, then
checks archive provenance, publication policy, and development cutover in local
fixtures. It creates no remote tags or releases. Missing staged dependencies,
copied or duplicated staging steps, and an unreported cutover must fail.

CI runs this rehearsal for release-related pull requests and every main push.
Its verdict is an owning CI check. This fixture proof does not replace candidate
archive certification or prove that live publication credentials work.

## Inspect platform evidence

Read each target's build, signing, packaging, attestation, and release-check
results in the candidate run. They must refer to the same candidate commit and
the files promotion will publish. A successful build alone does not prove that
the archive's release checks ran.

Record native workspace test results separately, with their workflow path,
tested commit, run and job identity, and actual verdict. Windows workspace
tests are advisory under
[their workflow policy](https://github.com/burin-labs/harn/blob/main/.github/workflows/windows-nightly.yml).
A failed Windows run or cancelled macOS run is not native passing evidence,
even when the release's required candidate checks pass. Use the owning
workflow policy to decide which checks gate publication.

Cache warming also has a distinct verdict. A successful warm-only build creates
no certified archives and must not be reported as a published candidate.

### Diagnose a source audit failure

Read the failed named job and its retained artifacts. Distinguish an assertion
failure from an unavailable prerequisite: a consumer waiting for a queued CLI
producer did not execute its source audit. Use the actual failure to choose the
narrowest check, then reuse the prerequisite once it is available.

For a Harn conformance failure, rerun the named file with the frozen candidate
binary and the release network environment cleared:

```bash
env -u HARN_EGRESS_ALLOW \
  -u HARN_EGRESS_DENY \
  -u HARN_EGRESS_DEFAULT \
  -u HARN_EGRESS_BLOCK_PRIVATE \
  -u HARN_EGRESS_ALLOW_LOOPBACK \
  HARN_BIN=/path/to/frozen/harn \
  ./scripts/harn_bin.sh -- test conformance --filter <case name>
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
