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
| `schema` | `harn.consumer_observer.v1` |
| `parent_repository` | Public parent repository |
| `parent_run_id` | Decimal workflow run identity |
| `parent_run_attempt` | Current workflow attempt |
| `parent_head_sha` | Immutable parent workflow revision |
| `source_revision` | Separately frozen source being rehearsed |
| `dispatch_nonce` | UUID identifying this dispatch |

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
lookup's 100-result bound. Partial or ambiguous matching results refuse ownership.

The action requires Node.js, `unzip`, and Actions read permission for both runs
and the parent's declaration. Artifact archives are limited to 64 KiB and must
contain only `consumer-observer.json`.

The action emits `owned=false` for an ordinary rehearsal. An admitted observer
emits `owned=true`, a receipt path, and the dispatch nonce. The consumer stores
that receipt in its own repository. Private child identities and artifacts
stay private.

The discovery name can locate a candidate; it cannot authorize cancellation.
An unreadable run or a missing identity refuses admission. A stale attempt,
ended parent, or unmeasured child state also refuses before expensive work. This action alone does not
cancel children or replace the observer's continuation windows.
