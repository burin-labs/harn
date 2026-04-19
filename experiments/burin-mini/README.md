# Burin Mini

Throwaway playground experiment for the DFS sub-agent research-queue architecture
tracked by `burin-code#144`.

This version stays deliberately small:

- one-screen Harn-native host in [host.harn](/Users/ksinder/.codex/worktrees/7fc6/harn/experiments/burin-mini/host.harn)
- one orchestration script in [pipeline.harn](/Users/ksinder/.codex/worktrees/7fc6/harn/experiments/burin-mini/pipeline.harn)
- one tiny TypeScript auth demo workspace under `workspace/`
- deterministic JSONL fixtures for the 3 canonical task shapes

## Canonical Tasks

1. `Explain this repo to me in simple terms`
2. `Comment what this file does`
3. `Add rate limiting middleware to the auth module`

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

## Deterministic Fixture Runs

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
```

## Live Ollama Runs

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
- `semantic_eval.json`: separate semantic grading result from [evaluator.harn](/Users/ksinder/.codex/worktrees/e04d/harn/experiments/burin-mini/evaluator.harn)
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

## Notes

- Reports are written to `experiments/burin-mini/evals/generated/<task-id>-latest.json`.
- Semantic evaluator helpers live in
  [lib/eval_common.harn](/Users/ksinder/.codex/worktrees/e04d/harn/experiments/burin-mini/lib/eval_common.harn),
  and the grader entrypoint is
  [evaluator.harn](/Users/ksinder/.codex/worktrees/e04d/harn/experiments/burin-mini/evaluator.harn).
- The verify script for the rate-limit task lives at
  [workspace/scripts/verify-rate-limit.sh](/Users/ksinder/.codex/worktrees/7fc6/harn/experiments/burin-mini/workspace/scripts/verify-rate-limit.sh).
- Repo integration:
  `cargo test -p harn-cli --test burin_mini_playground` exercises the paired
  playground host+pipeline flow, while `make lint-harn` checks the standalone
  host/lib modules and `make fmt-harn` checks formatting for the full
  experiment tree.
- Baseline comparison against current `burin-code` pipelines is documented at a
  qualitative level in [DECISION.md](/Users/ksinder/.codex/worktrees/7fc6/harn/experiments/burin-mini/DECISION.md).
