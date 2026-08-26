# Burin mini

Throwaway playground experiment for the DFS sub-agent research-queue architecture
tracked by `burin-code#144`.

This version stays deliberately small:

- one-screen Harn-native host in `host.harn`
- one orchestration script in `pipeline.harn`
- one tiny TypeScript auth demo workspace under `workspace/`
- deterministic JSONL fixtures for the 3 canonical task shapes

## Canonical tasks

1. `Explain this repo to me in simple terms`
2. `Comment what this file does`
3. `Add rate limiting middleware to the auth module`
4. `Add rate limiting middleware to the auth module with the slow verifier`

Task 4 is task 3 with a verifier that does not answer inline. See
[Slow verifiers and the command handle lifecycle](#slow-verifiers-and-the-command-handle-lifecycle).

The host and pipeline resolve those prompts onto the local sample workspace so
the experiment is reproducible from this clone.

## Run

From the repo root:

```bash
harn playground \
  --host experiments/burin-mini/host.harn \
  --script experiments/burin-mini/pipeline.harn \
  --task "Explain this repo to me in simple terms"
```

## Slow verifiers and the command handle lifecycle

Most verifiers in this playground return instantly, which hides a lifecycle that
real test suites always have: a command slower than its foreground budget does
**not** answer inline. The runner hands back a handle whose status is `running`,
and the exit status plus output arrive later, on the wait that resolves that
handle.

A loop that never performs that wait can watch a passing suite forever and never
learn it passed, because each fresh call starts another command and returns
another handle. That failure mode is invisible to a playground whose commands
all finish in milliseconds.

The `slow_verify_auth` profile makes it reachable. It declares
`background_after_ms`, which swaps the blocking `run` tool for a pair:

- `run` returns a handle with `status: running` for any command that outlives
  the budget
- `wait_command` turns that handle into an exit status and output

Profiles that do not declare `background_after_ms` keep the blocking `run` they
have always had, so the first three tasks are unaffected.

`tests/verify_channel_test.harn` pins the contract: a slow command really does
convert to a handle, waiting on it yields the real exit code and output, a
failing verifier comes back red, a fast command still answers inline in one
round trip, and resolving a handle costs what the command costs rather than a
multiple of the budget. Two of those cases go through the tool registry the
execute stage hands the model and recover the handle from the rendered result
the way a model must.

Note that a static `--llm-mock` fixture cannot express that last step: its tool
arguments are fixed before the handle exists, so it can never name the handle
the run just issued. The fixture below therefore runs the slow verifier and
stops at the handle. That is a faithful reproduction of the failure mode, not a
demonstration of the fix -- the run reports `verdict=pass` having never
collected the verifier's result.

## Deterministic fixture runs

```bash
harn playground \
  --host experiments/burin-mini/host.harn \
  --script experiments/burin-mini/pipeline.harn \
  --task "Explain this repo to me in simple terms" \
  --llm-mock experiments/burin-mini/fixtures/explain.jsonl

harn playground \
  --host experiments/burin-mini/host.harn \
  --script experiments/burin-mini/pipeline.harn \
  --task "Comment what this file does" \
  --llm-mock experiments/burin-mini/fixtures/comment.jsonl

harn playground \
  --host experiments/burin-mini/host.harn \
  --script experiments/burin-mini/pipeline.harn \
  --task "Add rate limiting middleware to the auth module" \
  --llm-mock experiments/burin-mini/fixtures/rate-limit.jsonl

harn playground \
  --host experiments/burin-mini/host.harn \
  --script experiments/burin-mini/pipeline.harn \
  --task "Add rate limiting middleware to the auth module with the slow verifier" \
  --llm-mock experiments/burin-mini/fixtures/slow-verify.jsonl
```

Running a fixture writes into the tracked sample workspace under `workspace/`.
Restore it with `git checkout -- experiments/burin-mini/workspace` (and delete
any file the run created) before committing.

## Live Ollama runs

`harn playground --llm ollama:<model>` sets the generator model. The semantic
evaluator defaults to the same provider/model unless you override
`BURIN_MINI_SEMANTIC_EVAL_PROVIDER` or `BURIN_MINI_SEMANTIC_EVAL_MODEL`.

```bash
HARN_LLM_TRANSCRIPT_DIR=$PWD/experiments/burin-mini/evals/live/explain/llm \
HARN_EVENT_LOG_DIR=$PWD/experiments/burin-mini/evals/live/explain/events \
harn playground \
  --host experiments/burin-mini/host.harn \
  --script experiments/burin-mini/pipeline.harn \
  --llm ollama:qwen2.5-coder:latest \
  --task "Explain this repo to me in simple terms"
```

Repeat that pattern for the comment and rate-limit tasks with a different output
directory.

For a single command that runs all 3 canonical tasks against isolated copies of
the sample workspace and stores per-task transcripts, events, reports, and
post-run workspaces under `evals/live/`:

```bash
./experiments/burin-mini/run_live_suite.sh qwen3.5:35b-a3b-coding-nvfp4
```

Each live task directory now contains:

- `report.json`: pipeline report emitted by `pipeline.harn`
- `run_record.json`: persisted action-graph run record when the task executed writes
- `semantic_eval.json`: separate semantic grading result from `evaluator.harn`
- `llm/` and `events/`: raw top-level transcripts plus sub-agent event logs used by the semantic grader
- `workspace_after/`: final sandbox workspace snapshot

For downstream consumers, treat `report.json` as the stable experiment API:

- `final_response`: the cleaned user-facing summary
- `visible_outputs`: per-stage user-facing summaries derived from Harn stage `visible_text`
- `research`: grounded fact records gathered through the queue

Treat the raw transcript files as chronology/debugging data instead:

- `llm/*.jsonl` includes wall-clock `timestamp`
- `events/*.jsonl` includes wall-clock `emitted_at_ms`

Set `BURIN_MINI_SEMANTIC_EVAL_MODE=heuristic` when you want a deterministic
local harness grade without spending an evaluator model call.

## Reasoning matrix

`run_reasoning_matrix.sh` runs the live suite across local and remote model
backends while varying Harn's provider-aware reasoning policy:

```bash
BURIN_MINI_SEMANTIC_EVAL_MODE=heuristic \
BURIN_MINI_MATRIX_POLICIES="off auto high" \
./experiments/burin-mini/run_reasoning_matrix.sh
```

The runner sources `~/projects/burin-code/.env` by default when it exists, but
never copies credentials into outputs. It probes local Ollama models, reuses an
already-running llama.cpp OpenAI-compatible server at `LLAMACPP_BASE_URL`
(`http://127.0.0.1:8001` by default), and filters Together candidates to
serverless coding-adjacent models whose advertised input and output prices are
no more than `$2/Mtok`. It ranks Qwen Coder routes before general reasoning
models because the playground stresses tool-following coding-agent behavior.
Override with:

- `BURIN_MINI_MATRIX_OLLAMA_MODELS`
- `BURIN_MINI_MATRIX_LLAMACPP_MODEL`
- `BURIN_MINI_MATRIX_TOGETHER_MODELS`
- `BURIN_MINI_MATRIX_INCLUDE_OLLAMA=0`
- `BURIN_MINI_MATRIX_INCLUDE_LLAMACPP=0`
- `BURIN_MINI_MATRIX_INCLUDE_TOGETHER=0`

Current empirical tuning notes: local Qwen3.6 over llama.cpp is the preferred
local serving route for this playground, and Harn's `auto` policy leaves
small/medium local Qwen tasks in no-thinking mode. Ollama Qwen3.6 raw-generate
requests were not stable enough in this harness during the May 2026 tuning pass.

## Notes

- Reports are written to `experiments/burin-mini/evals/generated/<task-id>-latest.json`.
- Semantic evaluator helpers live in `lib/eval_common.harn`, and the grader
  entrypoint is `evaluator.harn`.
- The verify script for the rate-limit task lives at
  `workspace/scripts/verify-rate-limit.sh`. The slow-verifier variant is
  `workspace/scripts/verify-slow.sh`; override its delay with
  `MINI_VERIFY_SLEEP_SECONDS`.
- Repo integration:
  `cargo test -p harn-cli --test burin_mini_playground` exercises the paired
  playground host+pipeline flow, while `make lint-harn` checks the standalone
  host/lib modules and `make fmt-harn` checks formatting for the full
  experiment tree.
- Baseline comparison against current `burin-code` pipelines is documented at a
  qualitative level in `DECISION.md`.
