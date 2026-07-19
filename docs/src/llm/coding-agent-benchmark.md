# Coding Agent Provider Benchmark

`harn eval coding-agent` runs a small, repeatable coding-agent fixture suite
across provider/model selectors and tool-call formats. The suite covers tiny
task shapes that stress harness quality without turning the command into a full
coding benchmark: one-file repair, CLI flag work, test-output-driven repair,
docs/example drift, read-only audit, and prompt-only diagnosis.

The default run is cost-free and deterministic:

```sh
harn eval coding-agent --model mock:mock --tool-format native,text
```

Artifacts are written to `.harn-runs/coding-agent-bench/latest/` by default:

- `summary.json`: aggregate pass/fail, token, cost, rollup, and native/text comparison data.
- `per_run.jsonl`: one normalized row per fixture/provider/tool-format run.
- `local_readiness.json`: local-provider recommendation evidence derived from
  local runs, with provider transport failures, unsupported capability
  failures, and behavioral task failures separated.
- `tool_mode_parity_overlay.toml`: generated `(provider, model)` parity verdicts
  with pass-rate, divergence, confidence, and evidence metadata for catalog promotion.
- `<run_id>/summary.json`: the Harn harness result for one run.
- `<run_id>/transcript_events.jsonl`: canonical transcript events from `transcript_events(...)`.
- `summary.md`: a readable table for sharing results.
- `followups.md`: candidate GitHub issues inferred from failures, rejected tool calls, or catalog gaps.

Run ids include the fixture id, model selector, and tool format, for example
`python-add__mock_mock__native`.

## Fixtures

Use `--fixture <id>` for tight local debugging, or `--fixture all` for the full
suite. `all` is the default.

| id | shape |
|---|---|
| `python-add` | one-file Python bug fix with verifier output |
| `cli-help-flag` | add a tiny CLI flag, update help/docs, and verify behavior |
| `test-output-first` | run failing tests before editing, then re-run them |
| `docs-symbol-rename` | update docs and an example after a symbol rename |
| `read-only-audit` | one-tool read-only audit with no edits |
| `no-tool-diagnosis` | prompt-only diagnosis with no tools |

## Structural Validator and Step Judge

The suite runs with the 4-rule structural validator by default. The validator
checks for empty replies when writes are expected, phantom completion, malformed
tool-call text, and suspiciously large prose-only output before a turn is
accepted. Use `--structural-validator off` to disable it for an ablation, or
`--structural-validator custom:<json>` to pass a literal
`with_structural_validator(...)` config.

`step_judge` remains opt-in for this benchmark. Pass `--step-judge
symmetric-cheap`, `--step-judge asymmetric`, `--step-judge symmetric-strong`,
or `--step-judge custom:<json>` when intentionally measuring an LLM judge. The
validator-vs-judge ablation in `experiments/step-judge/REPORT.md` found that
the validator catches useful text-format drift but does not replace the old
judge-on-text lift, so the benchmark default is validator-on and judge-off.

## Provider Matrix

Pass model selectors with repeated or comma-separated `--model` flags. Selectors
can be aliases, `provider:model`, or `provider=...,model=...`:

```sh
harn eval coding-agent \
  --fixture all \
  --model mock:mock,together:Qwen/Qwen3-Coder-30B-A3B-Instruct \
  --tool-format native,text \
  --replicates 2 \
  --env-file ~/path/to/provider.env \
  --max-runs 4
```

`--replicates` runs each fixture/model/tool-format cell independently and keeps
each transcript under a distinct run directory. The parity overlay counts every
native/text pair, so `--fixture all --replicates 2` meets the classifier's
two-replicate confidence floor when at least five fixtures complete in both
formats.

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

Turn the latest local benchmark output into a machine-readable recommendation
surface with:

```sh
harn provider catalog recommend --json
```

`harn provider catalog recommend --input <path>` accepts either `local_readiness.json`
or a raw coding-agent `summary.json`. Without an input path, it reads the latest
benchmark report and falls back to bundled seed evidence. `harn quickstart` uses
the same recommendation order when choosing among installed local Ollama models.

Use the same `summary.json` to refresh the generated provider support page and
JSON sidecar:

```sh
harn provider catalog support \
  --empirical .harn-runs/coding-agent-bench/latest/summary.json
```

The checked-in page stays deterministic when no empirical input is supplied, so
CI can run `make check-provider-support` without depending on local API keys.

To promote the latest parity verdicts into the provider capability matrix:

```sh
harn provider capabilities promote-from-eval \
  .harn-runs/coding-agent-bench/latest/tool_mode_parity_overlay.toml
```

## Reading Results

Use the rollups and native/text comparison table to spot provider abstraction
leaks:

- fixture rollups show which task shapes regress first.
- provider and model rollups show whether failures cluster by backend.
- tool-format rollups show whether native or text tool rendering is weaker.
- native passes while text fails, or the reverse, usually means the preset or
  provider adapter is exposing too much tool-channel behavior to harness authors.
- rejected tool calls followed by eventual success suggest Harn may need better
  transcript compaction, repair, or history-rewrite ergonomics for recoverable
  tool-call noise.
- provider transport failures such as HTTP 5xx, EOF, or connection resets are
  reported separately from model behavioral failures, so a runtime path does not
  get blamed on the model.
- unknown pricing on live models means the provider catalog cannot yet support
  credible cost recommendations.

The benchmark harness is intentionally simple. If it fails, blame the harness,
provider normalization, or preset defaults before blaming a cheap model.
