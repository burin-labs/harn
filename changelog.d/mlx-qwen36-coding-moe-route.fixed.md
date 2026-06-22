Fix the Apple-Silicon MLX local route so `auto`/presets land on real weights.
The MLX aliases (`mlx-qwen3.6-27b`, `mlx-qwen36-27b`, …) still pointed at
`unsloth/Qwen3.6-27B-UD-MLX-4bit` — the dense vision model that never finished
downloading (HF cache held only zero-byte `.incomplete` blobs) — even though
burin #2717 switched the launcher to the coding-tuned Qwen3.6-35B-A3B MoE served
via `mlx_lm.server`. Repoint every MLX alias (plus the `MLX_MODEL_ID` defaults
and the install guidance) to `unsloth/Qwen3.6-35B-A3B-UD-MLX-4bit` (q4) /
`-8bit` (q8), add the model rows with the shared `qwen3.6-35b-a3b`
logical_model / equivalence_group so eval aggregation compares the MLX and
llama.cpp runtimes directly, drop the stale `vision` capability (the MoE is
text-only), and carry `reserved_tool_call_token = true` on the MLX `*qwen3.6*`
capability row to match the Qwen3.6 tokenizer's reserved `<tool_call>` tokens.
The MLX runtime profile still requires a `tool_probe` before native is trusted;
if `mlx_lm.server` returns empty OpenAI tool_calls the safe pin is fenced-json.
