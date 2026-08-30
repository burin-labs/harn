# Execution decisions

`std/execution` records a small, replay-safe answer to “what should happen
next?” A decision is either `proceed`, `abstain`, or `escalate`. Its evidence is
a list of durable pointers, not copied prompts, tool payloads, or runtime
values.

```harn
import { decision_commit, evidence_ref, proceed } from "std/execution"

fn main(harness: Harness) {
  const decision = proceed(
    "deploy-42",
    "verification_passed",
    [evidence_ref("receipt", ".harn-runs/verify-42.json")],
  )
  const committed = decision_commit(harness.channels, decision)
  harness.stdio.println(committed.decision.outcome)
}
```

## Decision builders

| Function | Result |
|---|---|
| `proceed(id, reason_code, evidence?)` | Continue because the cited evidence supports the action. |
| `abstain(id, reason_code, missing, evidence?)` | Stop short of a claim and name the evidence that is missing. |
| `escalate(id, reason_code, target, evidence?)` | Hand the decision to a host-owned reviewer or role. |
| `evidence_ref(kind, ref, options?)` | Build a pointer with optional `label` and `hash`. |

Evidence kinds are `artifact`, `event`, `receipt`, `run`, `source`, and `trace`.
`reason_code` is a stable machine-readable reason, not hidden reasoning.

## Durable replay

`decision_commit(channels, decision, options?)` appends the decision to
`execution.decisions`. The decision id is the channel idempotency key. A replay
that commits the same id receives the first stored decision with
`duplicate: true`, even if its new candidate differs. This prevents a replay
from silently changing history.

The receipt also includes the active `execution_id`, channel name, and durable
event id. `scope`, `session_id`, `pipeline_id`, `tenant_id`, and a custom
`channel` can be supplied in `options`. Harn owns the durable decision record;
hosts still own approval presentation and the mutation performed after a
`proceed` decision.
