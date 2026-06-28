# Harn examples

Runnable `.harn` programs that show language and runtime features. Run any of
them with:

```bash
harn run examples/<name>.harn
```

New to Harn? Start with the zero-setup guided tour instead — it needs no API
keys and runs against checked-in LLM tapes:

```bash
harn demo            # menu of bundled scenarios
harn demo --list     # all scenarios with descriptions
```

## Running offline vs. live

Most examples either avoid the network entirely or are written to run against a
mock provider. To force offline mode for any example, set the mock provider:

```bash
HARN_LLM_PROVIDER=mock harn run examples/llm-call.harn
```

Examples that call a model (`llm_call`, `agent_loop`) produce real output only
when a provider is configured (e.g. `ANTHROPIC_API_KEY`, or a local model via
`harn local launch`); see [Getting started](../docs/src/getting-started.md).

## Map

- **Language basics** — `hello.harn`, `hello_v2.harn`, `fibonacci.harn`,
  `data-pipeline.harn`, `data-transform.harn`, `parallel-pipeline.harn`,
  `backslash_line_continuation.harn`.
- **LLM calls & middleware** (`std/llm/*`) — `llm-call.harn`,
  `llm_with_retry.harn`, `llm_handlers_pipeline.harn`, `llm_cost_budget.harn`,
  `llm_budget_aware.harn`, `agent_with_fallback.harn`, `llm_refine.harn`,
  `llm_best_of_n.harn`, `llm_ensemble.harn`, `llm_parallel_judge.harn`,
  `llm_pack.harn`.
- **Agents & tools** — `agent-loop-loop-until-done.harn`, `agent-pipeline.harn`,
  `multi-agent.harn`, `code-reviewer.harn`, `code_librarian_explore.harn`,
  `tool-hooks.harn`, `chatbot.harn`.
- **Sessions & context** — `session-fork.harn`, `session-multi-turn.harn`,
  `context-maintenance-demo.harn`, `persona-hooks.harn`.
- **Workflows & integration** — `eval-workflow.harn`, `mcp_server.harn`,
  `mcp-client.harn`, `sqlite-event-log-inspect.harn`, `portal-demo.harn`.

Subdirectories (`evals/`, `ui_resource/`, `triage/`, `dashboard_jobs/`,
`task_plan/`, `skill-packs/`, …) hold feature-specific and fixture-backed
examples referenced from the docs.
