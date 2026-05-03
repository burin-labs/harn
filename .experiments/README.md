# Codex-loop manual evals

Hand-run agent_loop scenarios against a local `llama-server` instance.
Used to empirically validate the four bug fixes in commit
`1439dd88` ("[codex] Fix four agent_loop bugs that broke Codex-style
workflows"). Not CI-gated — they require a local llama.cpp server and a
provider config.

## Setup

1. Start `llama-server` on `127.0.0.1:8080` with a model alias of
   `qwen3.6` (see `providers.toml`).
2. Build the harn binary: `cargo build --bin harn`.
3. Point harn at the experimental provider config and run an eval, e.g.:

   ```sh
   HARN_PROVIDERS_CONFIG=$(pwd)/.experiments/providers.toml \
     ./target/debug/harn run .experiments/exp2_sentinel_matrix.harn
   ```

   To capture the full request/response transcript for debugging, also
   set `HARN_LLM_TRANSCRIPT_DIR=/tmp/some-dir` — the run will write
   `llm_transcript.jsonl` there.

## What each script tests

| File | What it proves |
|---|---|
| `exp0_smoke.harn` | Single-tool calc loop terminates cleanly. Baseline sanity check. |
| `exp2_sentinel_matrix.harn` | Multi-step investigation (3 mock tools) terminates with `status=done` across all four `tool_format × done_sentinel` cells. The canonical regression for the bug bundle. |
| `exp2b_persistent.harn` | Same as exp2 but with `persistent: true`, to verify the `persistent` flag still controls only the text-only-turn break path (its actual semantic) and doesn't double-gate sentinel injection. |
| `exp5_single_variant.harn` | Single-variant run with a `post_turn_callback` that logs per-iteration tool calls. Combine with `HARN_LLM_TRANSCRIPT_DIR` to inspect what messages the model actually receives. |
| `exp6_optout_check.harn` | Sanity check on the three sentinel modes — default (`##DONE##`), opt-out (`""`), custom (`"STOP"`) — against the same minimal task. |

## Why these matter

The pre-fix runs of exp2 budget-exhausted at `max_iterations` in all
four variants because the loop was reading the *real* harn workspace
(custom `read_file` handler silently shadowed by the vm-stdlib
short-circuit) instead of the tiny mock data the script intended to
provide. Once user handler precedence was fixed, all four variants
terminate naturally in 4-6 iterations with a correct answer
(`compute(n) returns 2n+1, called as compute(5) in main → 11`).
