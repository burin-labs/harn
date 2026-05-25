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

## Three-strategy LLM head-to-head

`compare_strategies.harn` is the live counterpart to `eval.harn`: it
prompts a real model with the same five tasks under three planning
strategies (`baseline` prose, `burin_plan` first-order JSON, `typed_ir`
typed task-plan JSON) and emits one judge-free JSONL record per cell
with parse/validate/wall-clock metrics.

```sh
# Default: qwen3.6-coding (Ollama, free)
harn run examples/task_plan/compare_strategies.harn -- \
  --out=examples/task_plan/results/runs.jsonl

# Cross-check against a frontier API model
harn run examples/task_plan/compare_strategies.harn -- \
  --model=anthropic/claude-sonnet-4-6 \
  --out=examples/task_plan/results/runs_sonnet.jsonl

# Aggregate (overall + per-model + per-task tables)
harn run examples/task_plan/summarize_runs.harn -- \
  --in=examples/task_plan/results/runs.jsonl \
  --out=examples/task_plan/results/summary.md
```

`results/summary.md` in this directory is the head-to-head run that
backed the #2196 close recommendation; see the file for the per-model
breakdown across qwen3.6-coding (local Ollama), gpt-oss-120b
(Cerebras), and claude-sonnet-4-6 (OpenRouter).

[issue]: https://github.com/burin-labs/harn/issues/2196
