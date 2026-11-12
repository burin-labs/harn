# Release Harn

Publish a commit that already passed. Main always declares the next unpublished
stable version, so the four-hour checkpoint's candidate of main's tip can ship.
Nothing publishes until you ask. You need permission to dispatch
workflows in `burin-labs/harn`.

## Request a release with a pull request

Add `release-needed` before merging a pull request into main. Its merged event
starts `release-on-demand.yml`, which requests a fresh source candidate from
main and then promotes that exact candidate run. A main advance is accepted
only after proving that it contains the requesting merge. The candidate and
promotion workflows retain all existing qualification and publication gates.

The caller waits at most 210 minutes for the candidate and 120 minutes for
promotion. It reports the exact run IDs, observed job count, pending names and
failing names on the merged pull request. A failure, unreadable proof or deadline
also opens or updates one `release-blocked` issue for that request. Repair the
named cause, then manually start a new request with `dry_run=false`; never
partially rerun the candidate. A producer attempt above one is refused.
Successful recovery closes that request's blocked issue.

To inspect a merged request without dispatching builds, publication or comments:

```bash
gh workflow run release-on-demand.yml --repo burin-labs/harn --ref main \
  -f pull_request=PR_NUMBER -f dry_run=true
```

A dry run does not require the label and defaults to no effects. Its retained
`release-request-<run>-<attempt>` report contains dispatch plans. The optional
`inspect_candidate_run` input measures an existing candidate's exact first
attempt and job census through the same observer, separately from those plans.
It does not qualify that existing run for a newly planned source.

## Publish a release

Pick a main commit with a successful candidate, usually the newest one, and name
it:

```bash
gh workflow run promote-release.yml --repo burin-labs/harn --ref main -f candidate_sha=FULL_SHA
```

Promotion finds that commit's newest successful candidate run. To name another
run, add `-f candidate_run_id=RUN_ID`. It refuses, by name, unless all of these
hold:

- The commit is on main and its candidate run succeeded with every target's
  archive.
- [`green`](./dev/green-branch.md) contains the commit and the commit's full
  suite passed. A commit whose CI is still running waits up to 90 minutes.
- The commit declares a stable version with no published release, above the
  latest published one.
- Main's `CHANGELOG.md` doesn't record that version as `(never published)`.

An older commit can publish while main has moved on, under the same rules, as
long as its version is above the latest published one. No commit, version change,
or finished build starts promotion on its own; `release-needed` is an explicit
request through the caller above.

The candidate renders its notes from `changelog.d/` at build time and signs them
into its manifest; publication uses those exact notes. After publication,
`open-development-bump.yml` opens the version commit: main moves to the next
patch, and the published version's fragments fold into its `CHANGELOG.md`
section. `approve-release-pr.yml` supplies the review only when
`scripts/ci/approve_release_pr.sh` proves from the pull request's data that
`Start X.Y.Z development` changes nothing but version strings and that fold, and
posts it as `github-actions`, bound to the head SHA. Any other line in the diff
leaves the pull request waiting for a person.

## Repair a failed version commit

If publication completed but the post-publication version commit failed, repair
it through the same opener:

```bash
gh workflow run open-development-bump.yml --repo burin-labs/harn --ref main -f published_tag=vX.Y.Z
```

Omit `published_tag` to use the latest published stable release. The workflow
verifies publication before minting the release App token, derives the next
patch version, opens or reuses its pull request, validates its grammar
receipt, and enrolls a newly opened PR in Smart Ship. An existing PR retains
its owner's delivery grant, including an explicit stop. Resume a stopped PR
only after reading its label history and resolving the reason for the stop.
A repeated repair after the cutover lands is a no-op. Draft, prerelease, unreadable, and incomplete publication
records refuse the repair.

The monitor reports success only when main declares the expected next version
or a later one. An open update is pending, not successful convergence;
an unreadable, incomplete, or ambiguous PR census fails the measurement.

The five-minute repository-state controller automatically dispatches the same
opener once for an owed next version when no open cutover PR exists. A main
still declaring `X.Y.Z-dev` counts as owed, so it moves to `X.Y.Z` this way.
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

A source that can never promote does not strand its version: any later main
commit declares the same version and gets its own checkpoint candidate. Retire a
version only to abandon its identity on purpose. Read the main-push CI run at the exact source, then
request retirement through the existing development bump workflow:

```bash
gh workflow run open-development-bump.yml --repo burin-labs/harn --ref main \
  -f published_tag=vPREDECESSOR \
  -f retire_source_sha=FULL_SHA -f retire_release_pr=PR_NUMBER \
  -f retire_producer_run=PRODUCER_RUN -f retire_source_ci_run=CI_RUN
```

Replace the placeholders with the published predecessor and the source's records.
Omit every consumer-retirement input: mixing the two evidence paths is refused.
The planner decides from the source CI run's complete job census:

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
pull request that landed the source. Release-attempt refs other than that pull
request's must belong to a pull request that closed unmerged. It proves that the version has no tag or
release and that neither publication workflow is active, with known positive
reads on those paths. The opener repeats those checks after preparation before
opening its normal version PR. That PR records the version as `(never published)`
in `CHANGELOG.md`, which refuses any later promotion of it, and leaves its fragments
for the next version. This retires an unpublished identity; it does not publish
files from the candidate.

## Follow certification and publication

Follow the runs for the commit you named in
[Harn Actions](https://github.com/burin-labs/harn/actions).

1. `build-release-binaries.yml` first requires successful full-suite main CI
   at the exact candidate source, using promotion's job list in
   `scripts/lib/full_suite_jobs.sh`. It waits up to 85 minutes for a running
   suite and refuses missing or failed proof before building or signing. The
   gate emits
   `harn.release.full_suite.v1` JSON with the stage, source SHA, verdict,
   pending and failing job names, and next action. Native qualification starts
   after this admission and runs beside the artifact builds; publication still
   requires it. The four-hour checkpoint in `scripts/ci/green_checkpoint.sh`
   requests these source candidates at main's tip, and a maintainer can request one
   at any main commit. They qualify the consumer before promotion. Pushes to main
   only warm caches, whatever version they declare, and pull requests and merge groups
   build nothing. A successful candidate's archives stay available for 30 days.
   Candidate admission also requires the canonical dependency advisory scan at
   that exact source before expensive builds start. It checks both enabled
   features and the whole lockfile; a failed or missing scan refuses admission.
2. The candidate run builds, signs, notarizes, and attests the five platform
   archives. It checks those files with the release audit and smoke tests.
   The `candidate-manifest-<sha>` artifact binds the source commit, files,
   digests, and publication metadata. Keep its exact run ID with the release.
   The manifest carries the notes rendered from `changelog.d/`, or the existing
   `## vX.Y.Z` section when `CHANGELOG.md` has one.
3. The candidate runs the consumer rehearsal against its exact archives after
   native qualification and release smoke pass. A candidate cannot finish green
   without that measured consumer pass. `promote-release.yml` runs when you
   dispatch it and reauthenticates the candidate's rehearsal binding before
   publication. Earlier retained archives without a rehearsal use the same
   observer at promotion. That authenticated
   rehearsal waits for its producer's run ID and attempt, verifies the
   exact-source archive before installing it, and runs every required product
   check. It reads `Pairs-with: consumer#N` trailers from every commit
   between the previous stable tag and the release commit, so a feature
   commit's pairing selects that consumer pull request without a manual
   repair. Ordinary scheduled canaries and pull requests read only their own
   commit or explicit pull request description, even when the workspace version
   is stable. Two distinct pairings refuse and name both pulls. A paired pull
   already merged into the consumer's default branch rehearses that branch and
   logs `decision=rehearse_default`; a pull closed without merging refuses.
   A passing release rehearsal must retain `consumer-rehearsal-binding-<attempt>`;
   an unavailable manifest or consumer revision fails its recorder. The binding
   records the source SHA, producer run, candidate manifest digest, consumer
   run, rehearsed pull, and a digest of the consumer revision it tested.
   Promotion checks the version as [above](#publish-a-release), waits for
   `green`, and verifies the manifest, digests, and attestations. Planning and
   authorization each re-read the candidate's rehearsal
   binding and refuse by name when it doesn't describe this source, run, and
   manifest digest, when a paired pull's head moved or the pull closed, or when
   the tested consumer revision is no longer in the default branch's history.
   A default branch that advanced past the tested revision is logged, not
   refused. It also requires successful full-suite main-push
   CI at the release commit, including every deferred test family. It waits
   for an active run; missing or failed proof blocks publication. It creates
   the tag and GitHub release using those files. After qualification and before
   publication, the same canonical advisory scanner checks the certified source
   again against a freshly fetched database. A new advisory can therefore refuse
   an older, previously green candidate. Promotion doesn't rebuild its files.
4. The tag starts `publish-release.yml`, which checks the same full-suite proof
   before publishing crates from the tag.
   Promotion also packages the published Linux archives into the container
   and opens the next patch's version pull request.

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
owned Mac. Arbitrary branch sources
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

- The immutable tag selects the commit you named.
- The exact candidate and promotion runs succeeded.
- The GitHub release has all five archives, `SHA256SUMS`, and
  `release-assets.json`, with digests matching the candidate manifest.
- The tag's crate publication succeeded, and the versioned container is
  anonymously pullable.
- The post-publication version commit reached main or reported a proved no-op
  because main had already advanced.

Main ancestry, the declared version, and exact-source certification bind a tag to
its release source. A tag outside main also needs a trusted SSH signature naming
its candidate commit.

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

A failed candidate does not strand its version: the next checkpoint builds
another, and you can promote any commit that has one. Each candidate still
requires full-suite CI at its own source, so a red source never publishes.

- Candidate failed: you normally run nothing; the next checkpoint builds a new
  one. For a source defect, land the fix on main as an ordinary pull request and
  keep the version. To retry an infrastructure failure at the same source, rerun
  the failed jobs in that exact candidate run.
  Successful matrix legs stay complete. Candidate AOT payloads,
  license notices, and archives remain available for 30 days so a later retry
  can restore successful handoffs. Compiler caches are an acceleration aid and
  may be evicted independently.
- Promotion failed: rerun the failed jobs in the exact promotion run. It
  checks the existing tag's commit and refuses a conflicting tag.
- Promotion refused a stale rehearsal binding: for `consumer_pull_head_moved`,
  dispatch the explicit repair below with `-f consumer_pull_request=N`; it
  measures the pull's current head against the original archives. For
  `consumer_pull_merged_since_rehearsal`, `consumer_default_diverged`, or a
  mismatched source, producer, or manifest, name a newer candidate.
- A candidate's rehearsal failed at promotion: its failure isn't replaced without
  an explicit repair. Missing, failed, cancelled or mismatched results refuse
  publication.
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
  If that exact repair rehearsal passed but a later publication step failed,
  add `-f rehearsal_promotion_run_id=PRIOR_PROMOTION` together with the same
  `consumer_pull_request`. The reader authenticates the prior parent and its
  successful child, then rechecks the original producer, artifact identities,
  and current pull-request head. A changed, closed, failed, or unmeasured repair
  refuses rather than borrowing the old result.
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
- Container or version commit failed: rerun those failed promotion jobs.

Don't retag a published version or start a local publisher or watcher as a
second release controller. Retired Fleet launchers and candidate-build input
tuples aren't recovery entry points for these workflows.

## Check downstream convergence

For standalone Harn publication, dispatch the promoter with `candidate_sha` and
`standalone_harn=true`. This explicit
manual mode requires the same exact-source full-suite CI, native qualification,
successful build/smoke/audit stages, retained archives, signatures and
attestations. It independently authenticates the original producing attempt
and manifest ID/digest before verification and again before publication.
Missing, failed, expired or mismatched Harn proof refuses publication.

Standalone publication does not dispatch a consumer repair or consumer repins,
and cannot combine with either consumer recovery selector. It creates the
normal stable tag and latest GitHub release, which start crate publication;
the existing container and version-commit jobs also follow publication.
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
publication policy, and the version commit. It creates no remote release.
CI requires it for release changes and main pushes. It doesn't prove live
credentials or replace candidate certification.

For an offline audit or dry run, use:

```bash
./scripts/release_gate.sh audit
./scripts/release_gate.sh full --bump patch --dry-run
```

These local checks do not certify dependency security or authorize publication.
Live crate publication requires the hosted publisher and a fresh, signed proof
from the canonical dependency scanner for the exact published source. Recovery
at an existing tag runs that scanner again. A frozen tag workflow reads the
same verified promotion proof through its current staged release helpers.
