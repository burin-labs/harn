# Smart Ship

Smart Ship combines reviewed changes into immutable verification snapshots and
publishes a release checkpoint only when every required job has valid evidence.
The package runs on ordinary Git projects. Repository names, build commands,
execution environments, verification keys, and budgets belong to installation
policy.

[Install and control Smart Ship](docs/install.md).
[Run stock verification in CI](docs/ci.md).

The package provides signed verification and checkpoint contracts, durable Git
controller state, bounded repair claims, and a standard Harn repair agent. Live
forge admission and fleet cutover are tracked in
[harn#9857](https://github.com/burin-labs/harn/issues/9857).

The `github` export reads complete reviews and threads for one exact pull-request
head through the published GitHub connector. Live observations and archived
replay enter the same source and inventory contract. Missing actors, review
commits, and platform decisions remain unknown. This read-only interface does
not admit a change or perform a forge mutation.

`review_admission` qualifies complete live or archived review facts against an
installation's stable reviewer identities. Approval requires an independent
reviewer on the exact head, an open non-draft change, and resolved threads.
Missing facts remain pending; incomplete or differently bound evidence is
invalid. The result retains observed counts and named refusals. This qualifies
review only; job selection, required checks, and signed admission remain separate
obligations before integration.

`verify_admission` also requires an authenticated scope selection, every required
original or valid reused CI proof, and an installation-authorized admission
signature. Snapshots retain that complete record and requalify it during
checkpoint validation. A digest-shaped string cannot stand in for admission.
`prepare_admission` freezes precheck requests before execution; stock jobs with
reuse disabled must execute against those requests.
The request binds the signed scope, exact source and job inputs. An approval
that arrives after CI does not change that execution identity. The admission
issuer still signs the current complete review facts; a changed source, scope
or policy requires fresh qualification.

The GitHub export provides `github_ci_identities` and `assess_github_ci`.
The first projects the signed stock inventory into shared CI registration;
the second joins the original provider census with actual signed `JobRun`
artifacts. `assess_admission` uses the final verifier before an issuer key is
read. Assessment alone grants no admission authority. The
`admission-cli.harn` command reads live CI and current reviews, then optionally
issues a signed receipt. Its trusted issuer configuration and public policy
are separate from submitted evidence; a submission cannot select its own
signing authority. Current review is the last provider read before pure
requalification and signing. Hosted sandbox qualification remains in progress.

## Verify the package

```sh
harn package verify personas/smart_ship --strict
```

## Contract

Import `harn-smart-ship/lib` after installing the package. `verify` reports the
required job count and the names of pending, failed, infrastructure-failed, and
invalid jobs. Reused evidence retains the signed original run, attempt, job,
source, logs, and artifacts. Input, runtime, environment, coverage, and trust
identities must all match the candidate's requirement.

`prepare_job` measures the clean source and verification inputs without running
the test command. `freeze_snapshot` derives each job's dispatch request from the
complete frontier; `bind_job` connects the preparation to that request. Claim
the snapshot through `start`, then call `execute_job`. An execution's signature
binds its original request. An older job cannot be relabelled as a current
execution or copy its old request into a new frontier. `run_job` remains a
one-shot diagnostic check and does not claim a controller snapshot.

`promote` evaluates that evidence and the current controller fence. A stopped,
held, shadow, or stale controller receives a refusal. A draining controller can
finish already admitted verification while refusing new admission. Successful
assessment returns an immutable checkpoint; it does not mutate a branch or
deployment. `dispatch_reserved_effect` commits a unique dispatch claim through
the Git state's remote compare-and-swap before its privileged adapter contacts
the provider. The adapter keeps mutation credentials away from repair workers.
Every mutation owner must use this protocol before authority can transfer.
A prior read of a lease is insufficient.

`integration_effect` retains a complete cumulative snapshot for one repository.
Reservation reuses the snapshot verifier and admission budgets with a measured
current time. It does not mark the group integrated. After a durable dispatch
claim, `confirm_integration` prepares one state revision containing both the
confirmed effect and its pending verification work. The atomic forge adapter
must publish that state together with the destination change. Ordinary provider
callbacks, ordinary effect settlement and the ordinary Git state writer refuse
new integration confirmation, so they cannot silently lose the verification
obligation. Preparing the state is not evidence that a forge transaction happened.

The GitHub export adds `prepare_github_integration` and
`github_integration_transaction`. They retain the complete fresh review censuses
and build one transaction containing the destination update, the claimed
controller update, and an unchanged-ref guard for each newly admitted source
branch. The prepared controller object must contain the same witness and pending
verification obligation. Replayed data cannot drop or substitute these guards.
The claim retains the remote and ref measured by the Git owner. A known
controller frontier also binds the destination's expected base. Fork branches
cannot participate in this single-repository transaction, even when review
policy permits them. These functions prepare data without contacting the forge.
The Git store's `require_git_transport` checks fetch and push URL resolution
against the publisher's repository, using a private probe remote in the bare
object workspace. `stage_state` uploads the exact validated object to
`refs/heads/smart-ship-objects/<expected-authority-commit>` without moving the
authority ref. Competing states have the same parent and cannot replace one
another through its non-force push. Unknown uploads retain their ref and object
for reconciliation. The publisher must delete this ref with an exact-object
comparison only after the final transaction is reconciled.

`prepare_group_tree` composes exact reviewed heads with stock Git's
`merge-tree --write-tree`, including branches that forked before the destination
advanced. It writes only loose intermediate objects. `require_group_candidate`
then compares the complete result with the proposed candidate's actual tree and
requires exactly one parent at the expected destination. Missing or additional
edits and a changed parent are refused. These checks do not create a signed
candidate or prove that a provider accepted a ref update.

`dispatch_github_integration` consumes a provider-bound reservation, rechecks
complete current reviews and candidate materialization, uploads the exact state
object, then reads complete reviews again before making one SDK ref transaction.
A changed review census requires new preparation. A provider acknowledgement alone
cannot confirm integration: actual authority must retain the witnessed effect
and pending verification. Definite provider refusals settle through the same
effect owner. Preparation failures also settle a durable refusal before publication,
so rejected work does not retain a dispatch claim indefinitely. Unknown publication
outcomes require observation and never redispatch.

`recover_github_integration` reconstructs preparation from remote authority
history and the staging branch. Each dispatch records its immutable authority
revision, so later controls do not hide the original claim. Confirmed work remains
recoverable from history after staging cleanup; refusals retain the complete
transaction in their durable receipt. Oversized refusal details retain a transaction
digest and keep the staging object available for recovery. Early refusals have no
prepared transaction. `reconcile_github_integration` reads the
actual outcome. `cleanup_github_integration` deletes only the exact staged object
after conclusive confirmation or refusal, using another provider comparison.
Unresolved objects remain available for recovery.
Recovery and cleanup require the original dispatch's provider binding, including
its credential reference. A different reference cannot reconcile or delete its objects.

The adapter requires an already accessible signed GitHub candidate. Automated
signed candidate preparation and live GitHub qualification are still in progress.
The owning adapter tests run complete SDK requests against HTTP fixtures and real
Git ref transactions, including stop, head change, lost acknowledgement, lost
read-back, false acknowledgement, restart and cleanup. They do not prove GitHub's
live signature or protection enforcement.

`checkpoint_valid` verifies both the original execution signatures and the
checkpoint publisher's signature. Consumers use `release_identity` to obtain
the exact proven manifest identity. A moving branch is a convenience projection.

Scope adapters own complete job selection and execution-input identities. When
affected-test coverage cannot be established, they must require the full suite.
A signature authenticates the executor's report; it does not prove that its
test inventory covers the application. Incomplete and empty inventories fail.

`offer`, `start`, and `complete` coalesce pending cumulative work while preserving
the running snapshot. `claim_repair` reserves one bounded incident owner.
Each claim charges its declared spend allowance before dispatch. Measured
completion returns the unused allowance; expiry, stop, or missing usage retains
the full charge. `repair_spend` reports `spent_usd` as nil when actual spend is
unknown, lists every unreported attempt, and reports the conservative charge in
`committed_usd` to limit further attempts.
`repair_authorized` checks a generation-bound permit against freshly read state
before a worker effect. Stop revokes claims, including after resume. Transfer
requires drained workers and moves authority to a new generation in shadow mode.
The Git state adapter commits transitions with remote compare-and-swap. Stop
before dispatch commitment refuses the provider callback. An action already
committed for dispatch may finish; stop preserves its uncertain outcome until
an actual provider receipt is recorded. Unknown results block transfer and are
never redispatched from the original reservation.

The GitHub effect adapter reserves signed admissions through this same owner,
reads the current complete review and thread census, and applies the installation's
review policy before committing dispatch. A dismissed trusted approval, unresolved
thread, or missing census refuses the admission. The dispatch commitment rechecks
authority after those reads. The fresh source must match the signed source,
including its destination branch. The signed scope and installation rule bind
that destination independently. The published connector binds the merge to the
verified head; GitHub enforces its effective branch rules. Review reads and the
merge are separate API operations. GitHub does not provide an atomic review or
destination-branch guard for this merge call; a retarget after the final read
remains outside this adapter's guarantee.
Durable effect receipts retain the fresh review decision, canonical provider JSON,
and its checked hash, including the actual integrated commit. Duplicate
submissions return that recorded outcome without another provider call;
uncertain responses remain a transfer barrier. Configure and activate this
adapter only after qualifying the installation's release guard and exclusive
landing ownership.

The reservation also binds the merge method, API endpoint and credential reference.
Recovery checks that nonsecret identity before reading credentials or contacting a
provider, and the generic dispatch claim enforces the same binding. Rotating a
credential's value at its recorded reference does not put that value into state.

The Python and TypeScript fixtures execute ordinary commands against clean Git
sources. Each reviewed change passes independently; their combination fails one
test. `fixtures/live_repair.harn` uses a real configured model, independently
verifies the repair, and checks the signed checkpoint through the consumer
interface. Fixture review receipts are synthetic; these local examples do not
qualify GitHub review admission or a production controller.

Verification requires OS filesystem confinement to the job checkout and replaces
the inherited environment with the declared job environment. macOS and Linux
can supply that boundary; macOS requires kernel and System Integrity Protection
settings that enforce host-environment redaction. A host without the required sandbox backend refuses
execution; it cannot qualify a checkpoint from an unconstrained test run.
