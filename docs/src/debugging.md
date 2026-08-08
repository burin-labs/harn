# Debugging agent runs

Harn provides several tools for inspecting, replaying, and evaluating agent
runs. This page walks through the debugging workflow.

## Source-level debugging

For step-through debugging, start the Debug Adapter Protocol server. It speaks
DAP over stdio and is normally launched by an editor, but any DAP client can
drive it:

```bash
harn dap
```

(The standalone `harn-dap` binary alias and `cargo run --bin harn-dap` start the
same server — `harn dap` just makes it reachable with only `harn` on your PATH.)

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

### Privileged host-call bridge (`harnHostCall`)

The debug adapter advertises `supportsHarnHostCall: true` in its
`Capabilities` response. Trusted, provenance-stamped host bridge modules may
use the privileged `host_call(capability, operation, params)` wire; it is not a
general script API and cannot be imported or re-exported by ordinary modules.
When such a bridge call has no built-in handler, the adapter forwards it to the
DAP client as a **reverse request** named `harnHostCall` — mirroring the DAP
`runInTerminal` pattern:

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
 "output": "{\"call_id\":\"…\",\"model\":\"…\",\"input_tokens\":…,\"output_tokens\":…,\"cost_usd\":…,\"cache_read_tokens\":…,\"cache_write_tokens\":…,\"duration_ms\":…,\"iteration\":…}"}
```

IDEs can parse these to show a live LLM-call ledger alongside the
debug session. These are the same normalized accounting fields returned by
`llm_call`; the debugger does not re-price or rename them.

## Run views

Every `agent_loop(harness, ...)` or `workflow_execute()` call can produce a persisted run
under `.harn-runs/`. Inspect it through the stable `harn.run_view.v1` /
`harn.session_view.v1` projections rather than depending on private record
fields.

```bash
# List recent runs
ls .harn-runs/

# Inspect a stable run view
harn runs view --json .harn-runs/<run-id>.json
```

The view command shows a structured summary: stages executed, tools called,
token usage, timing, and final output.

## Correlating delegated runs

Build one report from the root run when several agents participated:

```bash
harn runs report .harn-runs/<root-run-id>.json > run-report.json
jq '.agents[] | {agent_id, status, usage, visible_output}' run-report.json
jq '.delegations, [.checks[] | select(.status != "passed")]' run-report.json
```

The report follows each typed `child_runs[].run_path`, checks the child's
back-pointer, and keeps the source hash beside the projected evidence. Add
`--events-db <path>` when the run used a SQLite event log. Canonical join
receipts make `coordination.unjoined` exact for terminal children and separate
the three costs a slow delegation can be paying: `observed_wait_ms` for
scheduler wait, `observed_join_ms` for terminal-to-collection lag, and
`observed_result_processing_ms` for the parent collapsing the result. All remain
`null` when event evidence is absent, malformed, or truncated, and a duplicate
receipt clears all three rather than only the lag. The report never turns
missing timing into zero.

Each timeline includes `coverage.returned`, `coverage.available`, and
`coverage.truncated`. Treat a missing event as evidence only when `truncated`
is `false`. A truncated run report also contains a `timeline_truncated` warning.
Query `harn.session_timeline.query` with a higher `limit` when you need more
nodes. `available: null` means Harn stopped after proving truncation, before it
could count every matching node.

Ask for a quick qualitative assessment after inspecting the deterministic
checks:

```bash
harn runs review --run-record .harn-runs/<root-run-id>.json \
  --events-db .harn/events.sqlite > run-review.json
jq '{verdict, confidence, findings, limitations, actions}' run-review.json
```

Use `--report run-report.json` instead when a report already exists. The two
inputs are explicit and mutually exclusive; Harn does not guess from a file's
contents. Use `--rubric rubric.md` to supply a project-specific rubric and
`--model <model>` to pin a model route. The review records both hashes and the resolved
route. It cites evidence by JSON Pointer and fails if a pointer does not resolve
inside the report. Coverage limits from the report remain explicit in the
review; the model cannot fill those gaps by reading other files. Harn projects
large arrays and strings into bounded first/last samples or previews before the
call. The review records every omission's original JSON Pointer, count, and
hash plus the source and projected byte counts, and repeats omissions as
deterministic limitations. If this auditable projection still exceeds the
48,000-token estimate, review fails before the model call.

## Comparing runs

Compare two stable views with your normal JSON diff tool to identify
regressions:

```bash
harn runs view --json .harn-runs/new.json > new.view.json
harn runs view --json .harn-runs/old.json > old.view.json
diff -u old.view.json new.view.json
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
const usage = harness.obs.llm_usage()
harness.stdio.log("Tokens used: ${usage.input_tokens + usage.output_tokens}")
harness.stdio.log("LLM calls: ${usage.total_calls}")
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
