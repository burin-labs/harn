# Coding Agent Provider Benchmark

`harn eval coding-agent` runs a small, repeatable coding-agent harness across
provider/model selectors and tool-call formats. The harness seeds a tiny Python
project with one failing unittest, runs the stdlib `repair_agent` preset with
read/edit/command tools, exports transcript JSONL, and verifies the repository
with `python3 -m unittest discover -s tests`.

The default run is cost-free and deterministic:

```sh
harn eval coding-agent --model mock:mock --tool-format native,text
```

Artifacts are written to `.harn-runs/coding-agent-bench/latest/` by default:

- `summary.json`: aggregate pass/fail, token, cost, and native/text comparison data.
- `per_run.jsonl`: one normalized row per provider/tool-format run.
- `<run_id>/summary.json`: the Harn harness result for one run.
- `<run_id>/transcript_events.jsonl`: canonical transcript events from `transcript_events(...)`.
- `summary.md`: a readable table for sharing results.
- `followups.md`: candidate GitHub issues inferred from failures, rejected tool calls, or catalog gaps.

## Provider Matrix

Pass model selectors with repeated or comma-separated `--model` flags. Selectors
can be aliases, `provider:model`, or `provider=...,model=...`:

```sh
harn eval coding-agent \
  --model mock:mock,together:Qwen/Qwen3-Coder-30B-A3B-Instruct \
  --tool-format native,text \
  --env-file ~/path/to/provider.env \
  --max-runs 4
```

Missing remote-provider credentials skip the run by default. Add
`--fail-on-unauthorized` when CI should fail instead. Environment values loaded
from `--env-file` are installed only for the process lifetime and are not written
to artifacts; the report records key names and source paths only.

## Local Models

Use `--include-local` to append reachable local runtime models from `harn local`
provider discovery:

```sh
harn eval coding-agent --include-local --max-local-models 1
```

Runs are serialized. For Ollama, Harn snapshots loaded models before each run and
unloads the evaluated model afterward only if the benchmark caused it to load.
Pass `--keep-local-after-run` to leave newly-loaded local models running.
Non-Ollama local servers are not killed unless Harn already owns a managed PID
through the `harn local` lifecycle commands.

## Reading Results

Use the native/text comparison table to spot provider abstraction leaks:

- native passes while text fails, or the reverse, usually means the preset or
  provider adapter is exposing too much tool-channel behavior to harness authors.
- rejected tool calls followed by eventual success suggest Harn may need better
  transcript compaction, repair, or history-rewrite ergonomics for recoverable
  tool-call noise.
- unknown pricing on live models means the provider catalog cannot yet support
  credible cost recommendations.

The benchmark harness is intentionally simple. If it fails, blame the harness,
provider normalization, or preset defaults before blaming a cheap model.
