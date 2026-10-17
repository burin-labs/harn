# Consumer observer admission

The `consumer-observer` composite action validates an optional observer before
a downstream rehearsal starts expensive work. A rehearsal without an observer
remains an ordinary run.

## Context

`scripts/lib/consumer-observer.mjs` owns context validation, the discovery name,
and child admission. Consumers use the action at an immutable commit rather
than copying its validator.

The closed context contains:

| Field | Meaning |
| --- | --- |
| `schema` | `harn.consumer_observer.v2` |
| `parent_repository` | Public parent repository |
| `parent_run_id` | Decimal workflow run identity |
| `parent_run_attempt` | Current workflow attempt |
| `parent_head_sha` | Immutable parent workflow revision |
| `source_revision` | Separately frozen source being rehearsed |
| `dispatch_nonce` | UUID identifying this dispatch |
| `started_at` | Original declaration Unix time; unchanged across observation windows |

Admission reads authenticated parent and child workflow metadata. It requires
the expected parent workflow, revision, current attempt, and live state. The
child must be the expected workflow dispatch by the trusted App, with the
expected source and the same context in its actual event inputs. The shared
owner fixes the dispatch actor to `harn-release-bot[bot]`; callers cannot change
that trust anchor through action inputs or environment variables.

Consumers must register the discovery projection on their workflow:

```yaml
run-name: >-
  ${{ inputs.observer
    && format('Consumer observer {0}.{1}/{2} @ {3}', fromJSON(inputs.observer).parent_run_id, fromJSON(inputs.observer).parent_run_attempt, fromJSON(inputs.observer).dispatch_nonce, fromJSON(inputs.observer).source_revision)
    || 'Consumer rehearsal' }}
```

This expression is mechanically checked against `observerRunNameExpression`.
The exact title binds discovery to the admitted dispatch; an ordinary workflow
title does not satisfy observer admission. The action cannot set a workflow's
run name.

Before dispatch, the parent stores the exact closed context as
`consumer-observer.json` in an immutable artifact named by
`observerDeclarationName(context)`. This public declaration contains no child
identity. Admission reads that artifact from the authenticated parent run,
checks its revision and archive digest, and requires the exact context. Missing,
duplicate, expired, or mismatched declarations refuse ownership. A fabricated
nonce for a real live parent is insufficient.

The artifact request filters by the exact declaration name. Its matching census
must be complete before admission; unrelated build artifacts do not consume the
lookup's bounded census. At most 1,000 matching results are read in complete
100-result pages. Partial, duplicate, changing, or over-budget results refuse ownership.

The action requires Node.js, `unzip`, and Actions read permission for both runs
and the parent's declaration. Artifact archives are limited to 64 KiB and must
contain only `consumer-observer.json`.

The action emits `owned=false` for an ordinary rehearsal. An admitted observer
emits `owned=true`, a receipt path, and the dispatch nonce. The consumer stores
that receipt in its own repository. Private child identities and artifacts
stay private.

The private v2 ownership receipt includes an admission-minted random
`receipt_nonce`. Terminal v2 receipts retain that same nonce. Only authenticated,
normalized custody is hashed into the public `custody_commitment`; the nonce
never leaves the private receipt. This binds recovery to one exact admitted
child without exposing guessable private run identities.

The discovery name can locate a candidate; it cannot authorize cancellation.
An unreadable run or a missing identity refuses admission. A stale attempt,
ended parent, or unmeasured child state also refuses before expensive work. This action alone does not
cancel children or replace the observer's continuation windows.

## Completion reconciliation

`consumer-observer-cleanup.yml` executes trusted default-branch code after one
of the three allowed parent workflows completes. It reads the exact historical
parent attempt, its complete job and artifact census, and its declarations.
Legacy parents with no declaration remain ordinary. The shared owner exports
the publication-step identity; workflow projections must use it exactly.
Completion reads that step's measured outcome, independently of reusable caller
prefixes on job names. A successful publication step whose artifact is missing,
or an unmeasured job-step census, refuses reconciliation. The number of
authenticated declarations must match the one successful publication step.
Multiple publishers, partial publication and artifacts left by failed
publication remain unproven. One declaration can still own multiple children.
The inert `consumer-observer-declare` action prepares and uploads that public
declaration. Callers must name its outer workflow step exactly
`Publish consumer observer declaration`; the action metadata is mechanically
checked against the shared owner. Dispatch consumes the action's closed context
only after publication succeeds.

`consumer-observer-dispatch.mjs` binds that context to its actual invoking run,
attempt and workflow revision. The shared dispatch owner authenticates the
immutable declaration and reads the latest live parent immediately before the
request. Parent reads use the job's public read token; the dispatch uses the
existing consumer-scoped App credential. A successful dispatch response exports
no private child identity. If the parent ends after the final read, downstream
admission still refuses expensive work.

Fresh canaries publish the declaration before dispatch. The existing shell
dispatcher calls the same admission owner through `--admit`, then passes its
validated context to the consumer. Observation keeps the declaration's original
start time. Authenticated historical reuse creates no declaration or new
ownership.

Only a newer unpinned scheduled canary supersedes its prior scheduled observer.
Manual runs have separate concurrency groups; release callers retain their
source-specific groups without cancellation. Parent completion then invokes
the existing receipt-bound cleanup below.

`reconcileOwnedChildren` discovers candidate children by the shared title, then
authenticates each private receipt and freshly reads that child's latest attempt.
The receipt must bind the same parent, declaration, source, workflow, App actor,
child revision, and child attempt. A newer child attempt refuses the old receipt.
Ordinary or reused rehearsals have no observer title and are preserved.

Only a live receipt-owned child of a completed parent receives a cancellation
request. Completion observation is bounded to six authenticated reads with five
seconds between readings. A request accepted by GitHub is counted separately
from measured termination. Pending or refused readings fail the listener;
neither missing custody nor an empty discovery census reports successful cleanup.
Multiple independently owned children are reconciled separately.
A final complete discovery read counts children that appeared during cleanup as
pending. This covers children visible at that final census. Discovery is not
atomic against a dispatch request still in flight, so a child appearing after
the final read remains a blind spot; dead-parent admission still refuses it.

GitHub's cancellation API has no atomic attempt precondition. A final fresh
child reading narrows the read/cancel race but cannot eliminate it. Reconciliation
prints only declaration and outcome counts, never private repository identities,
child run IDs, receipt contents, or arbitrary network errors. Candidate-build
and promotion dispatch callers retain their existing contracts.

## Product decision receipt

The `consumer-observer-terminal` action records the private consumer's reached
canonical decision. It reads the authenticated ownership artifact from its own
run, the parent's declaration and exact historical attempt, and a fresh child
reading. The producer's current run, attempt and revision must match that reading.
An ordinary rehearsal emits `recorded=false` without requiring a product file.

The canonical decision file contains exactly `source_revision`, `verdict`,
`owed`, `proven`, `pending`, `failed` and `failed_suites`. Recording requires the
observer's source, a positive owed count, no pending checks, and
`owed=proven+failed`. A pass has no failures and an empty `failed_suites`; a
failure has positive failures and exactly that many distinct suite identifiers.
Missing, skipped, empty or partial decisions refuse recording.
Harn validates this shape; the consumer owns which product checks are owed and
must emit the file only after its actual canonical judgment reaches every owed
check. A reached failure requires a source-qualified suite receipt and actual
producer failure. Setup, logging, skipped or missing evidence cannot manufacture
a product failure.

The action returns `recorded=true`, a private receipt path and the canonical
`artifact` name. Store the file in the consumer repository under that name,
`consumer-observer-product-<nonce>`. Its archive contains the single standard
filename `consumer-observer-<nonce>.json`. No child identity is published upstream.
Recording while the producer runs does not authorize release: a reader must
apply `qualifyTerminalReceipt` with the same authenticated ownership and parent
declaration. A pass requires fresh completed child SUCCESS; a reached product
failure requires fresh completed child FAILURE. Execution failure alone leaves
the product decision unreported.

`consumer-observer-observe.mjs` reads that decision through the same custody
owner used by cleanup. Its release `verdict` output exists only for `pass` or
`fail`. The separate `observation_outcome` also reports `pending`, `refused` or
`execution_failed`. A closed `reading` contains `discovered`, `owned`, `pending`, `refused`,
`custody_commitment`, `execution` and
`product`. Missing or ambiguous children and missing product receipts have
`product=null`; this is an unreported decision, not measured zero checks.
A pass requires exactly one discovered and authenticated child, a complete
private receipt and fresh completed SUCCESS. The returned product counts come
from the reached decision, never a private workflow job table. Final complete
discovery and fresh child reads narrow the race; GitHub provides no atomic
snapshot against a dispatch or rerun arriving after the final read.

`consumer-observer-read-decision.mjs` selects the original issued public context
without granting new ownership. It accepts a reached pass or failure only after
refreshing the parent identity, state and attempt, selected source eligibility,
complete child census and child attempt. A completed parent must retain its
conclusion; a live parent must remain live. The caller specifies whether the
consumer must still be on its current default source. An absent product receipt
does not authorize publication or retirement, even when execution failed.

`consumer-observer-read-parent.mjs` discovers an issued context by its public
parent and role. Its default operation requires a reached pass for historical
reuse; `--decision` returns a reached pass or failure through the same reader.
Both return `owned=false`. Reuse requires the current default consumer source
and retains the original context, rather than issuing another declaration.

The legacy canary and authenticated selector use `consumerPairing` for the
same trailer grammar. The legacy shell projects that owner through the
stdin-only `consumer-pairing.mjs` command with an environment containing only
`PATH`. The workflow installs the pinned Node runtime before its existing
dispatch. An absent trailer selects the default source; malformed, zero or
ambiguous pairings refuse the entire input.

Canary verdict observation and historical release recovery retain their
existing authenticated evidence. Fresh custody doesn't require a consumer
product receipt that the consumer hasn't adopted.

The declaration owner records `started_at` immediately before qualifying the
live parent and uploading its immutable declaration. The budget therefore
includes declaration qualification and upload, but excludes earlier candidate
builds. Callers cannot supply a separate start. Schema v2 replaces v1 directly;
no automatic caller had issued a production context before this cutover.

The bounded waiter keeps the original declaration start across credential renewal
windows. Window expiry can continue observation; aggregate expiry cannot become
a product pass or failure. Execution failure with no reached product decision
stops waiting through `execution_failed` and leaves the release verdict absent.
An expired renewal still performs one fresh authenticated observation. Only a
singular owned child with no reached product decision and no refused or failed
execution can produce a closed deadline receipt after 7,200 seconds. A late
product result, missing custody or infrastructure failure cannot produce it.
The deadline qualifier binds the issued start and observation time to the
canonical declaration and failed observation step intervals, followed by
successful publication in that same measured job. The descriptor contains the
public context, observation time, counts and opaque custody commitment, never
a private child identity or receipt nonce.
Deadline authentication and publication wiring remain unactivated.
The decision command's inert `--deadline` operation authenticates this original
receipt, its exact failed parent attempt, complete job census and canonical
producer steps. It refreshes the parent after reading the digest-bound artifact.
This historical evidence grants no product verdict or cleanup ownership.

The inert `--recover` operation combines that reached deadline with the shared
current-default product reader. The normalized private receipt must reproduce
the original deadline commitment, including its admission nonce, child run,
attempt and source. A different independently admitted child cannot replace
the original, even when that original disappears. Recovery requires a completed
successful child,
a reached pass and a complete exact-attempt job census with at least one actual
non-skipped job completed strictly after the original deadline. Every job must
belong to the same run, attempt and revision and have an accounted terminal
state. Missing timestamps cannot supply the completion witness. The reader
refreshes parent/source eligibility, discovery and child state after the timing
census. Child job identities and chronology remain private; the result retains
`owned=false`. The publication owner must separately qualify the producer's
source, consumer-only failure profile, archives and attestations.
The dispatch, decision and waiter commands remain inert until their owning
workflow callers are integrated and the operational lifecycle is qualified.
