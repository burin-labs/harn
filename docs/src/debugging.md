# Debugging Agent Runs

Harn provides several tools for inspecting, replaying, and evaluating agent
runs. This page walks through the debugging workflow.

## Source-level debugging

For step-through debugging, start the Debug Adapter Protocol server:

```bash
cargo run --bin harn-dap
```

In VS Code, the Harn extension contributes a `harn` debug configuration
automatically. The equivalent `launch.json` entry is:

```json
{
  "type": "harn",
  "request": "launch",
  "name": "Debug Current Harn File",
  "program": "${file}",
  "cwd": "${workspaceFolder}"
}
```

This supports line breakpoints, variable inspection, stack traces, and step
in / over / out against `.harn` files.

## Run records

Every `agent_loop()` or `workflow_execute()` call can produce a run record —
a JSON file in `.harn-runs/` that captures the full execution trace including
LLM calls, tool invocations, and intermediate results.

```bash
# List recent runs
ls .harn-runs/

# Inspect a run record
harn runs inspect .harn-runs/<run-id>.json
```

The inspect command shows a structured summary: stages executed, tools called,
token usage, timing, and final output.

## Comparing runs

Compare a run against a baseline to identify regressions:

```bash
harn runs inspect .harn-runs/new.json --baseline .harn-runs/old.json
```

This highlights differences in tool calls, outputs, and token consumption.

## Replay

Replay re-executes a recorded run, using the saved LLM responses instead of
making live API calls. This is useful for deterministic debugging:

```bash
harn replay .harn-runs/<run-id>.json
```

Replay shows each stage transition and lets you verify that your pipeline
produces the same results given the same LLM responses.

## Visualizing a pipeline

When you want a quick structural view instead of a live debug session, render a
Mermaid graph from the AST:

```bash
harn viz main.harn
harn viz main.harn --output docs/main.mmd
```

The generated graph is useful for reviewing branch-heavy pipelines, match arms,
parallel blocks, and nested retries before you start stepping through them.

## Evaluation

The `harn eval` command scores a run or set of runs against expected outcomes:

```bash
# Evaluate a single run
harn eval .harn-runs/<run-id>.json

# Evaluate all runs in a directory
harn eval .harn-runs/

# Evaluate using a manifest
harn eval eval-suite.json
```

### Custom metrics

Use `eval_metric()` in your pipeline to record domain-specific metrics:

```harn
eval_metric("accuracy", 0.95, {dataset: "test-v2"})
eval_metric("latency_ms", 1200)
```

These metrics appear in run records and are aggregated by `harn eval`.

### Token usage tracking

Track LLM costs during a run:

```harn
let usage = llm_usage()
log("Tokens used: ${usage.input_tokens + usage.output_tokens}")
log("LLM calls: ${usage.total_calls}")
```

## Portal

The Harn portal is an interactive web UI for inspecting runs:

```bash
harn portal
```

This opens a dashboard showing all runs in `.harn-runs/`, with drill-down
into individual stages, tool calls, and transcript snapshots.

## Tips

- **Add `eval_metric()` calls** to your pipelines early — they're cheap to
  record and invaluable for tracking quality over time.
- **Use replay** for debugging non-deterministic failures: record the failing
  run, then replay it locally to step through the logic.
- **Compare baselines** when refactoring prompts or changing tool definitions
  to catch regressions before they ship.
