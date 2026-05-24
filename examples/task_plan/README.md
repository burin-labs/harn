# Task-plan IR fixtures

JSON fixtures for the experimental typed task-plan IR
(`std/agent/task_plan`). Each fixture maps to one of the five evaluation
tasks from [harn#2196][issue]:

| File | Task |
|---|---|
| `plans/01_rate_limiter.json` | Add a rate limiter around existing auth middleware |
| `plans/02_config_option.json` | Add a config option plus tests (with `sub_agent` fan-out + `join`) |
| `plans/03_failing_test.json` | Diagnose and fix a failing test (deterministic reproduce + diagnose + fix) |
| `plans/04_doc_rename.json` | Update docs and examples after an API rename (`workflow_map` fan-out) |
| `plans/05_cross_file_refactor.json` | Small cross-file refactor with verifier output, human gate, and compact step |

Together they exercise every IR node kind: `read_fact`, `search`,
`context_pack`, `agent_loop`, `sub_agent`, `workflow_map`, `verify`,
`human_gate`, `deterministic_command`, `join`, `compact`.

## Validate and compile

```sh
harn run examples/task_plan/eval.harn -- --plans=examples/task_plan/plans
```

Emits one JSONL record per fixture covering validation, compile success,
and the lowered `WorkflowGraph`'s own `workflow_validate` result.

```sh
harn run examples/task_plan/eval.harn -- \
  --plans=examples/task_plan/plans \
  --out=runs/task_plan_eval.jsonl
```

The driver is deterministic and does not call any LLM. See
[`docs/src/task-plan-ir.md`](../../docs/src/task-plan-ir.md)
for the IR contract and the three-strategy comparison protocol from #2196.

[issue]: https://github.com/burin-labs/harn/issues/2196
