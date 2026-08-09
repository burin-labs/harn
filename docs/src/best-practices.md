# Best practices

This guide collects the habits that keep Harn programs small, testable, and
easier to operate.

## Keep prompts narrow

The best prompts are short and explicit. Tell the model exactly what shape of
output you want, what to avoid, and when to say it does not know something.
Prefer one task per call over one giant prompt that tries to do everything.

## Use explicit context

Pass the minimum useful context into each model call. If the model only needs a
few files or a short patch, read those directly instead of dumping the entire
repository into the prompt.

## Prefer typed boundaries

Use type annotations, shape types, and small helper functions where they make
the interface clearer. A narrow typed boundary is easier to debug than a
large pile of implicit dicts.

## Make concurrency obvious

Use `parallel each` when the work is independent and order matters. Use
`parallel` when you need indexed fan-out. Keep the body of each worker short so
it is obvious what is happening concurrently.

## Record metrics early

If a pipeline matters enough to keep, add `eval_metric()` calls sooner rather
than later. Track the numbers you will want during regressions: accuracy,
latency, token usage, and counts of failures or retries.

## Fail fast on unclear inputs

Use `require`, `guard`, typed catches, and explicit validation when the pipeline
depends on a particular shape of data. It is cheaper to fail immediately than
to let a bad input travel through several stages.

## Keep operational surfaces small

For MCP servers, host integrations, and agent tools, expose only the minimum
surface you need. Smaller tool surfaces are easier to document, secure, and
debug.

## Inspect before you scale

Use `harn repl` for quick experiments, `harn viz` for structural overviews,
`harn doctor` for environment checks, and `cargo run --bin harn-dap` through
the DAP adapter when you need line-level stepping.

## Recommended workflow

For a new agent or pipeline:

1. Prototype the prompt in `harn repl`.
2. Turn it into a named pipeline.
3. Add a small example under `examples/`.
4. Add metrics or a conformance test.
5. Use `harn viz` and the debugger when the control flow gets complicated.

That sequence is usually enough to keep the implementation honest without
turning the repository into a framework project.
