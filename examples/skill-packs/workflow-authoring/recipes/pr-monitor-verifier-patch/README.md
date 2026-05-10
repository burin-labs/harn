# PR-monitor verifier patch

Worked example of a workflow patch that **modifies an existing bundle**
rather than rewriting it. Apply on top of the PR-monitor recipe to
insert a deterministic verifier between `query_logs` and `notify`, plus
a repair branch that loops back to verify on failure.

## What the patch does

| Op | Effect |
|---|---|
| `insert_node verify_logs`  | Adds an `action` node that asserts the previous summary cites file:line + an error string. |
| `insert_node repair_logs`  | Adds an `agent` node that fixes the summary if verification fails. |
| `add_edge query_logs -> verify_logs` | Routes the existing query output through the new verifier. |
| `add_edge verify_logs -> notify [verify_pass]` | On success, hand off to the existing notification node. |
| `add_edge verify_logs -> repair_logs [verify_fail]` | On failure, run the repair agent. |
| `add_edge repair_logs -> verify_logs` | Loop back to re-verify the repair output. |
| `upsert_prompt_capsule verify-logs` | Attach the verifier's prompt capsule. |

## Run it

```bash
harn workflow patch validate \
    --bundle ../pr-monitor/bundle.json \
    --patch  patch.json \
    --json

harn workflow patch apply \
    --bundle ../pr-monitor/bundle.json \
    --patch  patch.json \
    --out    /tmp/pr-monitor-with-verifier.bundle.json
harn workflow validate --bundle /tmp/pr-monitor-with-verifier.bundle.json
```

The validator should report the patched bundle as valid; the structural
diff should list `verify_logs` and `repair_logs` as added nodes plus
the four new edges; and `capability_delta.widening` should be empty
(this patch only adds nodes/edges, it does not raise the autonomy tier
or expand any capability).

## Capability ceiling

To verify that this patch does *not* widen the parent ceiling, supply a
`--parent-ceiling` JSON file that describes the active execution
policy:

```bash
harn workflow patch validate \
    --bundle ../pr-monitor/bundle.json \
    --patch  patch.json \
    --parent-ceiling ../../../../docs/fixtures/workflow-bundles/parent-act-with-approval.policy.json \
    --json
```

(See the workflow-authoring SKILL.md "Workflow patch authoring"
section for the full contract.)
