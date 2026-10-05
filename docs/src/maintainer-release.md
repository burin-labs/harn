# Release Harn

Use Harn's GitHub workflows to open a release, certify its files, and publish
them. You need permission to dispatch workflows in `burin-labs/harn`.

## Open the release pull request

Confirm that `main` declares an `X.Y.Z-dev` workspace version and contains
unreleased `changelog.d/<id>.<category>.md` fragments. Then run:

```bash
gh workflow run bump-release.yml --repo burin-labs/harn --ref main
```

The opener takes no version or candidate-build inputs. It strips the declared
`-dev` suffix, folds the fragments, regenerates derived files, and opens
`Release vX.Y.Z`. It publishes one GitHub-signed commit and arms squash
auto-merge through the merge queue. Required checks and review still apply.

The daily schedule runs the same decision. A stable workspace version or no
unreleased fragments produces `action=none`, with a notice explaining why.
An unreadable pull-request list fails instead of opening a duplicate.

The signed commit is recorded at `release-attempt/vX.Y.Z/<commit>` before the
pull request opens. An existing release stays frozen at that attempt. Later
fragments remain for the next release, including when their changes merge ahead
of this release. A push changing `changelog.d` checks the existing attempt
without rebuilding or opening a release.

An existing `release/vX.Y.Z` branch without a matching attempt fails closed.
To replace an unrecorded stale attempt, close its pull request and dispatch the
opener explicitly. Inspect the replacement head's required checks and review.
Do not reset an attempt ref or dispatch a second version selector.

## Repair a failed development bump

If publication completed but the post-publication development bump failed,
repair it through the same opener:

```bash
gh workflow run open-development-bump.yml --repo burin-labs/harn --ref main -f published_tag=vX.Y.Z
```

Omit `published_tag` to use the latest published stable release. The workflow
verifies publication before minting the release App token, derives the next
development identity, opens or reuses its pull request, validates its grammar
receipt, and arms the normal merge queue. A repeated repair after the cutover
lands is a no-op. Draft, prerelease, unreadable, and incomplete publication
records refuse the repair.

The five-minute repository-state controller automatically dispatches the same
opener once for an owed development version when no open cutover PR exists.
It verifies publication and reserves the attempt on the fixed commit in
`scripts/development_cutover_repair.json` before dispatch. Scheduled and manual
controller checks share that reservation owner. Existing PRs, including red
ones, remain their owner's work.

If reservation or dispatch fails, inspect the controller run and use the manual
repair command above. The reservation deliberately prevents another automatic
attempt, including after a crash. Missing or incomplete reservation history
refuses automatic repair. Do not change the fixed anchor or seed to clear a
reservation, and do not infer completed integration from a reserved attempt.

## Follow certification and publication

Record the release pull request and the exact commit that lands on main.
Follow the runs for that commit in
[Harn Actions](https://github.com/burin-labs/harn/actions).

1. `build-release-binaries.yml` builds the candidate in its merge group.
   The main push reuses a successful candidate for that exact commit, or builds
   it when no reusable queue candidate exists. Other pushes only warm caches.
   Daily scheduled runs build signed source candidates at their exact main
   commit, including commits outside the warm-cache path filter. They reuse a
   successful candidate only while its manifest, release files, and every
   target archive remain available and unexpired. These runs do not publish a
   release.
2. The candidate run builds, signs, notarizes, and attests the five platform
   archives. It checks those files with the release audit and smoke tests.
   The `candidate-manifest-<sha>` artifact binds the source commit, files,
   digests, and publication metadata. Keep its exact run ID with the release.
3. `promote-release.yml` starts after the successful main push run. It finds
   the successful candidate run at that commit and verifies the manifest,
   digests, and attestations. It creates the tag and GitHub release using those
   files. Promotion doesn't rebuild them.
4. The tag starts `publish-release.yml`, which publishes crates from the tag.
   Promotion also packages the published Linux archives into the container
   and opens the next patch's development-version pull request.

Publication is complete only after you verify all of these:

- The release pull request merged, and the signed tag selects its main commit.
- The exact candidate and promotion runs succeeded.
- The GitHub release has all five archives, `SHA256SUMS`, and
  `release-assets.json`, with digests matching the candidate manifest.
- The tag's crate publication succeeded, and the versioned container is
  anonymously pullable.
- The post-publication development bump reached main or reported a proved
  no-op because main had already advanced.

Read [Release assets manifest](./dev/release-assets-manifest.md) for the
download contract. A visible tag or release page alone doesn't prove complete
publication.

## Recover a failed run

Read the failing job before choosing a retry. Keep the commit, run ID, and
candidate manifest attached to the release record.

- Opener failed before publication: fix the cause and dispatch
  `bump-release.yml` again. Its admission checks run again.
- Candidate failed because of infrastructure: rerun the failed jobs in that
  exact candidate run. A source defect needs a corrected pull request and
  certification of the resulting commit.
- Promotion failed: rerun the failed jobs in the exact promotion run. It
  checks the existing tag's commit and refuses a conflicting tag.
- Candidate succeeded but no main-push event started promotion: dispatch the
  existing promoter on main with that certified candidate's run ID:
  `gh workflow run promote-release.yml --repo burin-labs/harn --ref main -f candidate_run_id=RUN_ID`.
  It refuses an unsuccessful or foreign producer and a source not contained
  in main, then applies the normal version, consumer, manifest, digest and
  attestation checks. It publishes existing files without rebuilding them.
- Crate publication failed after the tag exists: rerun the failed jobs in the
  tag's `publish-release.yml` run. Its publisher resumes remaining crates.
- Container or development bump failed: rerun those failed promotion jobs.

Don't retag a published version or start a local publisher or watcher as a
second release controller. Retired Fleet launchers and candidate-build input
tuples aren't recovery entry points for these workflows.

## Check downstream convergence

Promotion's `repin` job dispatches the registered consumers' own update
workflows. For the fleet-owned bump adapters that is one dispatch: the
harn-bump-fleet `promote-released-orchestration.yml` workflow moves the
orchestration pin to the release, and each adapter's converged landing starts
that consumer's bump. Each consumer opens its own pull request, then follows its
checks, review, and merge queue. Record each consumer's terminal state separately from
Harn publication. Dispatch success doesn't prove that a consumer updated or
that its pull request merged.

## Rehearse a release change offline

Run the fixture rehearsal with an installed Harn binary:

```bash
HARN_BIN="$(command -v harn)" bash scripts/release_rehearsal.sh
```

The rehearsal runs publication staging and checks archive provenance,
publication policy, and development cutover. It creates no remote release.
CI requires it for release changes and main pushes. It doesn't prove live
credentials or replace candidate certification.

For an offline audit or dry run, use:

```bash
./scripts/release_gate.sh audit
./scripts/release_gate.sh full --bump patch --dry-run
```
