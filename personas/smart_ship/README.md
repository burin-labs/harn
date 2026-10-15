# Smart Ship

Smart Ship combines reviewed changes into immutable verification snapshots and
publishes a release checkpoint only when every required job has valid evidence.
The package runs on ordinary Git projects. Repository names, build commands,
execution environments, verification keys, and budgets belong to installation
policy.

[Install and control Smart Ship](docs/install.md).

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
