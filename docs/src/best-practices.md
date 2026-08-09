# Best practices

These habits make Harn programs easier to understand, test, and operate.

## Give each layer one job

- Use `harness.llm.call` for one model request.
- Use `agent_loop` when the model must act across turns.
- Use a workflow for named stages, joins, and verification.
- Keep product UI, approval decisions, file mutation, and persistence in the
  host that owns them. See [the host boundary](./host-boundary.md).

## Keep inputs and prompts small

Pass the context that the current step needs. Ask for one clear result. State
the output shape, limits, and failure behavior in the prompt or schema.

## Make effects explicit

Route model, file, network, and process access through `harness.*`. Keep pure
transforms in ordinary functions. Give an agent only the tools and capability
scope that it needs.

## Make concurrency readable

Use `parallel each` for independent work. Give each worker a clear input and
join the results at a visible point. Set limits before you add fan-out.

## Treat completion as a contract

Do not treat a model's confident sentence as proof that work is complete. Use a
typed result, a verification stage, or an explicit terminal condition. Record
the evidence that a critical action ran.

## Test in two modes

Use the `mock` provider for deterministic syntax, error, and control-flow
tests. Run a small number of real-provider smoke tests for provider wiring and
model capability. These prove different things.

Before you commit, run:

```bash
harn fmt <file-or-directory>
harn check <file-or-directory>
harn lint <file-or-directory>
```

Use [Testing](./testing.md) for fixtures, replay, evaluation, and evidence
standards.
