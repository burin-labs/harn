# Decision

Status: scaffolded, fixture-driven, and live-smoke-tested against local Ollama.

## What Worked

- `harn playground` is now good enough for deterministic control-flow iteration
  because it accepts `--llm-mock` and `--llm-mock-record`, not just `harn run`.
- `sub_agent_run(...)` is the right primitive for research workers and executor
  workers. The clean parent transcript shape matches the experiment's goal.
- The simplest useful architecture is explicit orchestration code plus typed
  sub-agent envelopes. No extra framework layer was needed.
- A tiny in-repo workspace makes the three task shapes reproducible from a
  fresh clone.

## What Did Not Work Yet

- I do not have a fresh, apples-to-apples numeric baseline against current
  `burin-code` pipelines for the exact same three prompts. Recent eval work in
  `burin-code` exists, but not as a clean side-by-side experiment on these
  exact tasks.
- Live Ollama behavior is now characterized qualitatively from full JSONL
  transcripts, but it is still environment-dependent and not yet stable enough
  to treat the 3-task suite as a hard pass/fail release gate.

## Latest Live Snapshot

Model: `qwen3.5:35b-a3b-coding-nvfp4` via local Ollama on April 19, 2026.

- `explain_repo`: pass in the full live suite
- `comment_file`: pass in the full live suite
- `rate_limit_auth`: still unstable in live runs

Observed `rate_limit_auth` failure modes across transcript-backed reruns:

- planner over-researches and fails to converge to a concrete action list even
  after it already has enough facts
- planner converges, but the executor still misses verifier-exact identifier
  requirements such as `rateLimit` versus `rateLimiter`

This is good enough to validate the architecture and the playground/toolchain
integration, but not good enough to call the third task solved on a cheap local
model without more context-engineering and runtime support.

## High-Level Comparison

| Dimension | Burin Mini DFS queue | Current burin-code pipeline |
| --- | --- | --- |
| Research boundary | Explicit `sub_agent_run` workers with narrow tool scope | More integrated, less obviously rip-apart-able |
| Parent transcript cleanliness | Strong | Mixed, depends on task path |
| Planner completeness gate | Explicit and typed | Present in different forms, but more pipeline-specific |
| Executor parallelism | Simple dependency-batch fan-out | Stronger overall runtime, more productized |
| Deterministic iteration | Very good with playground + fixtures | Strong eval harness, but heavier to iterate in |
| Product readiness | Low, intentional throwaway | High, production path |

## Recommendation

Recommendation: adopt specific components, not a wholesale replacement.

Adopt:

- `playground + --llm-mock` as the fastest harness-engineering loop
- `sub_agent_run` research workers with strict tool narrowing
- explicit plan-completeness gating as a typed envelope
- separate evaluator model call as a first-class end-of-run step
- transcript-backed debugging as the default workflow for cheap-model failures

Do not adopt wholesale:

- this exact pipeline as a production replacement for `burin-code`
- the in-repo sample workspace approach outside experiments

## Follow-Up

- Use the transcript-backed failures from `rate_limit_auth` to drive runtime and
  stdlib follow-up issues around plan normalization, verifier-aware planning,
  and structured worker result synthesis.
- If a clean `burin-code` baseline is needed later, run the same 3 prompts
  against `burin-agent --project <repo> "<task>"` and append the comparison.
