# Release Harn

Use Harn's GitHub workflows to open a release, certify its files, and publish
them. You need permission to dispatch workflows in `burin-labs/harn`.

## Open the release pull request

Releases are cut from [`green`](./dev/green-branch.md), the newest `main`
commit whose full CI suite passed. Each green-checkpoint pass that finds
unreleased `changelog.d/<id>.<category>.md` fragments on a `green` that declares
an `X.Y.Z-dev` workspace version opens the release by itself, so you normally
run nothing. Fragments that landed on `main` after green wait for the next
release. To open one at once, run:

```bash
gh workflow run bump-release.yml --repo burin-labs/harn --ref main
```

The opener takes no version or candidate-build inputs. It strips the declared
`-dev` suffix, folds the fragments, regenerates derived files, and opens
`Release vX.Y.Z`. It publishes one GitHub-signed commit and adds `ship` so
Smart Ship owns delivery through the merge queue. Required checks and review
still apply: `approve-release-pr.yml` supplies the review only when
`scripts/ci/approve_release_pr.sh` proves from the pull request's data that the
bot's `Release vX.Y.Z` or `Start X.Y.Z-dev development` diff changes nothing but
version strings and the changelog fold, and posts it as `github-actions`, bound
to the head SHA. Any other line in the diff leaves the pull request waiting for
a person. Repeated openers leave an existing PR's delivery grant unchanged.

The opener restores the Harn binary that green's own CI run built, verified
against green's commit, instead of compiling a second one. It builds only when
that run's artifact has expired. The green checkpoint, the daily schedule and a
manual dispatch all run the same decision. A stable workspace version or no
unreleased fragments produces `action=none`, with a notice explaining why.
An unreadable pull-request list fails instead of opening a duplicate.

The signed commit is recorded at `release-attempt/vX.Y.Z/<commit>` before the
pull request opens from that ref. A mergeable release stays frozen at that
attempt. New fragments remain for the next release, including when they merge
ahead of this release. A push changing `changelog.d` checks existing attempts
before restoring or building Harn.

If a fragment already folded into the attempt is edited on main, Git reports a
modify/delete conflict. Conflicts in outputs that release preparation regenerates
are recoverable too; `scripts/generated_artifacts.toml` owns both their commands
and their conflict scope. The opener waits until green contains the exact current
blobs, then prepares a new signed, immutable attempt from that green snapshot.
The replacement contains green's code and folds green's complete fragment set,
including later fragments, so its notes describe the same new cut as its code.
It enrolls the replacement in Smart Ship and closes the conflicted PR with a
link to the replacement. It preserves the earlier commit and attempt ref.
Other conflicts, missing attempt records, and multiple mergeable attempts fail
closed instead of choosing a source.

If replacement cleanup fails after the new PR opens, dispatch the same opener
again. It selects the prepared replacement and finishes closing the superseded
PR without rebuilding, republishing, or changing the existing PR's `ship`
grant. The `release-opener-<run>-<attempt>` artifact records the recovery plan,
selected source, outcome, and next action, including when an invocation fails.

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
receipt, and enrolls a newly opened PR in Smart Ship. An existing PR retains
its owner's delivery grant, including an explicit stop. Resume a stopped PR
only after reading its label history and resolving the reason for the stop.
A repeated repair after the cutover lands is a no-op. Draft, prerelease, unreadable, and incomplete publication
records refuse the repair.

The development monitor reports success only when main declares the expected
development version. An open update is pending, not successful convergence;
an unreadable, incomplete, or ambiguous PR census fails the measurement.

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

## Retire an unpublished candidate whose source can never promote

A source that can never promote no longer strands its version: a later main
commit that declares the same version gets its own candidate, and promotion
publishes the first one that qualifies. Retire a version only to abandon its
identity on purpose. Read the main-push CI run at the exact source, then
request retirement through the existing development bump workflow:

```bash
gh workflow run open-development-bump.yml --repo burin-labs/harn --ref main \
  -f published_tag=vPREDECESSOR \
  -f retire_source_sha=FULL_SHA -f retire_release_pr=PR_NUMBER \
  -f retire_producer_run=PRODUCER_RUN -f retire_source_ci_run=CI_RUN
```

Replace the placeholders with the published predecessor and the owning release
records. Omit every consumer-retirement input: mixing the two evidence paths
is refused. The planner decides from the source CI run's complete job census:

| Source CI run | Candidate build | Decision |
| --- | --- | --- |
| Failed with a failed owning job on attempt 2 or later | Any terminal conclusion | Retire |
| Failed on attempt 1 | Any | Refuse: main CI recovery still retries it |
| Succeeded without every full-suite job succeeding | Any terminal conclusion | Retire |
| Succeeded with every full-suite job | Any terminal conclusion | Refuse: the source can still promote |
| Failed only in derived aggregate jobs | Any | Refuse: unmeasured |
| Not completed, cancelled, or incomplete census | Any | Refuse: unmeasured |
| Any | Not completed | Refuse: unmeasured |

A failed candidate build under a green full suite is refused because rerunning
the build can still promote the source. Main CI recovery reruns a first-attempt
failure once, so only a failure that survives that retry is persistent. The full-suite job list lives in
`scripts/lib/full_suite_jobs.sh`, shared with the promotion gate.

The planner also requires exact source identity, main ancestry and the merged
release PR. Release-attempt refs other than the merged PR's must belong to a
release PR that closed unmerged. It proves that the version has no tag or
release and that neither publication workflow is active, with known positive
reads on those paths. The opener repeats those checks after preparation before
opening its normal development-version PR. Follow that PR to main, then use the
release opener for the next version. This retires an unpublished identity; it
does not publish files from the candidate.

## Follow certification and publication

Record the release pull request and the exact commit that lands on main.
Follow the runs for that commit in
[Harn Actions](https://github.com/burin-labs/harn/actions).

1. `build-release-binaries.yml` first requires successful full-suite main CI
   at the exact candidate source, using promotion's job list in
   `scripts/lib/full_suite_jobs.sh`. It waits up to 85 minutes for a running
   suite and refuses missing or failed proof before building, signing, or
   dispatching the consumer rehearsal. The gate emits
   `harn.release.full_suite.v1` JSON with the stage, source SHA, verdict,
   pending and failing job names, and next action. Native qualification starts
   after this admission and runs beside the artifact builds; publication still
   requires it. The workflow then builds the candidate on the version commit's
   push to main. The release's merge group builds nothing; its
   `Release candidate verdict` passes only after the exact-source consumer
   rehearsal passes at the commit that lands. A merge group can't build the
   candidate because the trusted macOS smoke host admits only
   `refs/heads/main`. Other pushes only warm caches.
   Daily scheduled runs build signed source candidates at their exact main
   commit, including commits outside the warm-cache path filter. They reuse a
   successful candidate only while its manifest, release files, and every
   target archive remain available and unexpired. While main declares a
   stable version with no published release, every source candidate, scheduled
   or requested, is a release candidate of that version: it runs the consumer
   rehearsal and native qualification, and promotion can publish it.
2. The candidate run builds, signs, notarizes, and attests the five platform
   archives. It checks those files with the release audit and smoke tests.
   The `candidate-manifest-<sha>` artifact binds the source commit, files,
   digests, and publication metadata. Keep its exact run ID with the release.
   Its consumer rehearsal waits for successful archive and manifest jobs,
   then passes that producer's run ID and attempt with the authenticated
   observer. The consumer verifies the exact-source archive before installing
   it and still runs every required product check. Rehearsals without archives
   keep their source or pull-request CLI path. This installation permission
   does not replace the terminal consumer verdict or publication checks.
3. `promote-release.yml` starts after every successful main run of the
   producer, whatever event started it. When the run's source declares a
   stable version whose transition its first-parent history proves, it finds
   the successful candidate run at that source, waits up to 90 minutes for
   `green` to contain the source, and verifies the manifest, digests, and
   attestations. A version already published from another candidate is a
   no-op. It also requires successful full-suite main-push
   CI at the release commit, including every deferred test family. It waits
   for an active run; missing or failed proof blocks publication. It creates
   the tag and GitHub release using those files. Promotion doesn't rebuild them.
4. The tag starts `publish-release.yml`, which checks the same full-suite proof
   before publishing crates from the tag.
   Promotion also packages the published Linux archives into the container
   and opens the next patch's development-version pull request.

Before promotion, the candidate's exact source needs a successful
`macos-nightly.yml` run through `workflow_dispatch`. The candidate run starts
it: `scripts/ci/dispatch_native_qualification.sh` dispatches the main revision of
the workflow with the candidate's `source_sha`. The producer waits up to
85 minutes for the native proof beside the archive builds. Failed, missing,
or unreadable proof blocks the consumer rehearsal and candidate verdict,
instead of waiting until promotion. The run preserves
`harn.release.native_qualification.v1` JSON with the source SHA, verdict,
pending and failing job names, and next action.
Before executing candidate code,
the trusted workflow verifies that this source is a main ancestor and measures
both allowed execution and a refused write through process confinement on the
owned Mac. Scheduled source candidates start it only while main owes a release. Arbitrary branch sources
are refused before checkout or execution on the restricted runner group.

The common publication gate requires the native workspace tests, CLI smoke and
test-case performance steps to have executed successfully in that exact attempt.
It reports named pending or failed conditions and refuses absent, partial,
skipped or foreign evidence. A run-scoped artifact binds the tested CLI source,
workflow revision, repository, run, attempt and phases. A display title alone
cannot qualify a candidate. Missing or mismatched receipts refuse publication.
Qualification dispatches never publish their older source as a main warm cache.
Windows native qualification remains advisory.
The platform policy lives in `scripts/lib/release_platforms.harn`; its JSON
projection in `scripts/release_audit_contract.json` is checked mechanically.
Hosted audit receipts carry the downloaded artifact as the macOS platform's
`qualification` field and retain the workflow's `head_branch` and policy
`head_sha` in its run record. The audit contract checks that same source, run,
attempt and phase binding before reusing proof.

Publication is complete only after you verify all of these:

- The release pull request merged, and the immutable tag selects its certified source.
- The exact candidate and promotion runs succeeded.
- The GitHub release has all five archives, `SHA256SUMS`, and
  `release-assets.json`, with digests matching the candidate manifest.
- The tag's crate publication succeeded, and the versioned container is
  anonymously pullable.
- The post-publication development bump reached main or reported a proved
  no-op because main had already advanced.

Main ancestry and exact-source certification bind a tag to its release source.
A tag outside main also needs a trusted SSH signature naming its candidate commit.
Corrected stable sources also prove the original version transition in their
first-parent history, a verified source signature, and durable publication
evidence. The published manifest and signed archive index must bind all five
archive digests to that exact source and the owning producer's workflow, main
ref, run, and attempt. This read does not depend on temporary build artifacts
or retained workflow-run records. The signed index proves provenance; the
public release supplies publication authority. Its author and the required
manifest, index, and archive uploaders must be the owning release app's GitHub
bot identity; additional SDK assets can come from their own publisher.

Read [Release assets manifest](./dev/release-assets-manifest.md) for the
download contract. A visible tag or release page alone doesn't prove complete
publication.

## Recover a failed run

Read the failing job before choosing a retry. Keep the commit, run ID, and
candidate manifest attached to the release record.

Transient observer read failures retry against the same consumer run within
its original budget. Credential renewal keeps the run, start time, deadline,
and any server-requested backoff. The `consumer-observer-checkpoint-<attempt>-<source>`
artifact records the last observation and next action. Unknown timing is
`null`; a checkpoint never supplies a product verdict or publication permission.
After an observer fails, cleanup preserves a live child unless the parent was
explicitly cancelled or a fresh authenticated job census proves a timeout.
An unreadable census refuses cancellation and reports pending or refused work.
The same cleanup workflow revisits live children every fifteen minutes, so work
preserved by a completion listener is checked again after its deadline. A
scheduled run discovers live children first, then authenticates their original
receipts and parent attempts through the same cancellation gate. Scheduling can
be delayed by GitHub; it does not extend a release's observation budget.

A failed candidate does not strand its version. While main declares an
unpublished stable version, every main run that leaves nothing to promote runs
`scripts/ci/release_candidate_reconcile.sh`. When no candidate build or
candidate promotion is in flight and main's newest commit has had no source
candidate, it requests one with the release App; promotion then publishes the
first candidate of the version that qualifies. Each candidate still requires
full-suite CI at its own source, so a red source never publishes. The
reconciler requests at most one source candidate per main commit. When main's
newest commit has already had one, nothing is in flight, and the version commit
is more than four hours old, promotion runs fail and one
`[Release] vX.Y.Z is unpublished on main` issue opens. Close it after the
release publishes.

- Opener failed before publication: fix the cause and dispatch
  `bump-release.yml` again. Its admission checks run again.
- Candidate failed: you normally run nothing. The next main push requests a
  fresh source candidate of main, and its success promotes automatically. For
  a source defect, land the fix on main as an ordinary pull request and keep
  the version. To retry an infrastructure failure at the same source, rerun
  the failed jobs in that exact candidate run; its success also promotes
  automatically. Successful matrix legs stay complete. Candidate AOT payloads,
  license notices, and archives remain available for 30 days so a later retry
  can restore successful handoffs. Compiler caches are an acceleration aid and
  may be evicted independently.
- Promotion failed: rerun the failed jobs in the exact promotion run. It
  checks the existing tag's commit and refuses a conflicting tag.
- Candidate succeeded but no completed main run started promotion: dispatch the
  existing promoter on main with that certified candidate's run ID and source:
  `gh workflow run promote-release.yml --repo burin-labs/harn --ref main -f candidate_run_id=RUN_ID -f candidate_sha=FULL_SHA`.
  Ordinary recovery refuses an unsuccessful or foreign producer and a source
  not contained in main, then applies the normal version, manifest, digest and attestation
  checks. If the producer never ran a consumer rehearsal, recovery runs the
  existing rehearsal workflow at that exact source and requires its measured
  pass before publication. Missing, failed, cancelled or mismatched results
  refuse publication. Without an explicit repair, a producer's existing failed
  rehearsal is not replaced.
  Recovery publishes the existing files without rebuilding them.
  When the consumer needs a prepared repair before it can land against the
  published tag, add `-f consumer_pull_request=PR_NUMBER`. Recovery selects
  that open pull request in the registered consumer repository through the
  existing pairing checks; a fork, closed pull, malformed number, or conflicting
  source pairing refuses. Consumer observation and publication checks still
  run. This selection cannot reuse an earlier default-branch rehearsal.
  An explicit repair can recover a waiting or failed consumer stage when every
  original build, smoke, audit and required archive handoff succeeded. Recovery
  checks exact-source full-suite CI and native qualification before dispatching
  the new rehearsal. Its custody and terminal receipts bind the original
  manifest artifact ID and digest, and publication reauthenticates that same
  evidence and current repair head. Missing, expired or failed archive proof
  refuses. The original candidate run stays unchanged.
- The consumer observer reached its deadline, but its exact dispatched child
  later succeeded: after the completed-child recovery policy is on main,
  dispatch the normal promoter with the same producer and source plus
  `-f completed_consumer_run_id=CHILD_RUN_ID`. The authorization job verifies
  the original dispatch and deadline, every producer build/smoke/audit result,
  the child's complete successful census and product receipts, then rechecks
  both latest attempts. Only the observer deadline and its derived final
  verdict may have failed. Cancelled, failed, partial, replaced or mismatched
  children refuse. The shared publisher authenticates the original observer
  context, deadline and reached product before applying all normal archive and publication
  checks. The original deadline remains unmeasured; no result is rewritten
  and no equal-tree source substitution is accepted.
- Crate publication failed after the tag exists: rerun the failed jobs in the
  tag's `publish-release.yml` run. Its publisher resumes remaining crates.
  If publication policy needed a repair on main, dispatch `publish-release.yml`
  with the immutable `tag`, its `expected_source_sha`, and the landed
  `expected_policy_sha`. Recovery uses that policy without changing the tag's source.
- Container or development bump failed: rerun those failed promotion jobs.

Don't retag a published version or start a local publisher or watcher as a
second release controller. Retired Fleet launchers and candidate-build input
tuples aren't recovery entry points for these workflows.

## Check downstream convergence

For standalone Harn publication, dispatch the existing promoter with
`candidate_run_id`, `candidate_sha`, and `standalone_harn=true`. This explicit
manual mode requires the same exact-source full-suite CI, native qualification,
successful build/smoke/audit stages, retained archives, signatures and
attestations. It independently authenticates the original producing attempt
and manifest ID/digest before verification and again before publication.
Missing, failed, expired or mismatched Harn proof refuses publication.

Standalone publication does not dispatch a consumer repair or consumer repins,
and cannot combine with either consumer recovery selector. It creates the
normal stable tag and latest GitHub release, which start crate publication;
the existing container and development-bump jobs also follow publication.
It does not claim consumer compatibility or adoption. Record and complete those
separate obligations through the owning consumer workflows.

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
