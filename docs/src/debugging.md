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

### Host-call bridge (`harnHostCall`)

The debug adapter advertises `supportsHarnHostCall: true` in its
`Capabilities` response. When a script calls `host_call(capability,
operation, params)` and the VM has no built-in handler for the op, the
adapter forwards it to the DAP client as a **reverse request** named
`harnHostCall` — mirroring the DAP `runInTerminal` pattern:

```json
{"seq": 17, "type": "request", "command": "harnHostCall",
 "arguments": {"capability": "workspace", "operation": "project_root",
               "params": {}}}
```

The client replies with a normal DAP response:

```json
{"seq": 18, "type": "response", "request_seq": 17, "command": "harnHostCall",
 "success": true, "body": {"value": "/Users/x/proj"}}
```

On `success: true`, the adapter returns the body's `value` field (or the
whole body when `value` is absent) to the script. On `success: false`,
the adapter throws `VmError::Thrown(message)` so scripts can `try` /
`catch` the failure like any other Harn exception. Clients that do not
implement `harnHostCall` still work — the script just sees the
standalone fallbacks (`workspace.project_root`, `workspace.cwd`, etc.).

### LLM telemetry output events

During `run` / step-through, the adapter forwards every `llm_call` the
VM makes as a DAP `output` event with `category: "telemetry"` and a
JSON body:

```json
{"category": "telemetry",
 "output": "{\"call_id\":\"…\",\"model\":\"…\",\"prompt_tokens\":…,\"completion_tokens\":…,\"cache_tokens\":…,\"total_ms\":…,\"iteration\":…}"}
```

IDEs can parse these to show a live LLM-call ledger alongside the
debug session.

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
