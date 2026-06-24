# Provider tool-call wire-format sweep — 2026-06-24

Empirical, real-spend forced-format probe of the Harn provider catalog. Goal:
for each `(provider, model)` route we hold a key for, establish which of
`{native, json, text}` tool-call wire formats actually *dispatches a parseable
call* and *carries a backslash-heavy body byte-clean*, and which combos to
avoid.

This sweep follows the 2026-06-21 methodology documented in burin-code
`docs/blog-drafts/the-tool-call-dialect-problem.md`: a single-tool authoring
call under a forced `tool_format` that bypasses the capability gate, scored on
(a) **dispatch** (did a parseable tool call come back) and (b) **fidelity**
(does the authored file body round-trip byte-exact).

## Method

- **Probe binary**: worktree-built `harn` (v0.8.139, branch
  `ksinder/toolmode-sweep-fill`), isolated `CARGO_TARGET_DIR`. Probe harness in
  `.probe/probe.py` (not committed).
- **native**: raw HTTP `POST /chat/completions` with `tools` +
  `tool_choice: "required"` (falls back to `"auto"` where a provider rejects
  `required`, e.g. DashScope-backed Qwen on OpenRouter), extract
  `choices[0].message.tool_calls[0].function.arguments`, JSON-parse, byte-compare
  the `body` field.
- **json** / **text**: raw HTTP with NO `tools`; a system prompt instructs the
  model to emit one ` ```tool ` fenced `{name,args}` block (json) or a
  `<tool_call>name({ ... })</tool_call>` heredoc call (text). The raw completion
  is fed to the **actual** `agent_parse_tool_calls(raw, tools, fmt)` builtin —
  the same parser the agent loop uses — so fidelity is measured against Harn's
  real decoder, not a Python re-implementation.
- **Fidelity body** (the discriminator): a backslash-heavy Zig source with
  `\\` multiline-string prefixes, a Windows path `C:\app\data\config.json`, and
  an embedded JSON string with `\"key\"` and `C:\\temp\\x`. Trailing-newline
  stripping is tolerated (models routinely strip it; it is not a wire-format
  defect). Any backslash/quote escaping drift fails fidelity.
- **Samples**: N=2 reconnaissance across all routes; N=5 confirmation on every
  route flagged for a pin change (per the meter-stick N>=5 bar).

## Scope

Per the task hand-off, **gpt-oss and GLM-5.x rows are owned by a sibling PR
(Harn-FootgunGate) and were NOT edited here.** Any gpt-oss/GLM findings below
are reported for that owner only.

## Results — N=2 reconnaissance (dispatch/fidelity per cell)

`d/f` = dispatch count / fidelity count out of 2.

| route | model | native d/f | json d/f | text d/f |
|---|---|---|---|---|
| or-qwen3.7-max | qwen/qwen3.7-max | 0/0 (tool_choice=required rejected*) | 2/2 | 2/2 |
| or-qwen3.7-plus | qwen/qwen3.7-plus | 0/0 (required rejected*) | 2/1 | 2/2 |
| or-kimi-k2.7-code | moonshotai/kimi-k2.7-code | 2/0 | 0/0 | 2/2 |
| or-deepseek-v3.2 | deepseek/deepseek-v3.2 | 2/2 | 2/2 | 1/1 |
| or-kat-coder-pro-v2 | kwaipilot/kat-coder-pro-v2 | 2/1 | 2/2 | 2/2 |
| or-step-3.7-flash | stepfun/step-3.7-flash | 0/0 (empty) | 1/0 | 0/0 |
| or-gemma4-31b | google/gemma-4-31b-it | 2/0 | 2/2 | 2/1 |
| tog-deepseek-v4-pro | deepseek-ai/DeepSeek-V4-Pro | 2/2 | 2/2 | 2/2 |
| tog-minimax-m2.7 | MiniMaxAI/MiniMax-M2.7 | 2/0 | 2/2 | 2/1 |
| tog-gemma4-31b | google/gemma-4-31B-it | 1/0 | 0/0 | 2/2 |
| minimax-m2.7 (direct) | MiniMax-M2.7 | 2/2 | 1/1 | 2/2 |
| minimax-m3 (direct) | MiniMax-M3 | 1/1 (markup) | 1/1 | 2/1 |
| di-deepseek-v4-pro | deepseek-ai/DeepSeek-V4-Pro | 2/2 | 2/2 | 2/2 |
| di-kimi-k2.7-code | moonshotai/Kimi-K2.7-Code | 2/2 | 1/1 | 2/2 |
| di-qwen3.6 | Qwen/Qwen3.6-35B-A3B | 1/1 (empty) | 0/0 | 2/2 |
| sn-deepseek-v3.2 | DeepSeek-V3.2 | 2/2 | 2/2 | 2/2 |
| sn-minimax-m2.7 | MiniMax-M2.7 | 2/0 | 2/0 | 2/2 |
| sn-llama-3.3-70b | Meta-Llama-3.3-70B-Instruct | 2/0 | 2/0 | 2/2 |
| nim-nemotron-super | nvidia/nemotron-3-super-120b-a12b | 1/1 (markup) | 1/0 | 2/2 |
| nim-nemotron-nano | nvidia/nemotron-3-nano-30b-a3b | 0/0 (markup) | 1/0 | 2/1 |
| bt-kimi-k2.7-code | moonshotai/Kimi-K2.7-Code | 2/2 | 2/2 | 2/2 |
| bt-deepseek-v4-pro | deepseek-ai/DeepSeek-V4-Pro | 2/2 | 0/0 | 2/2 |
| bt-nemotron-super | nvidia/Nemotron-120B-A12B | 2/2 | 2/2 | 2/2 |

\* `or-qwen3.7-*` native returned HTTP 400 only because the DashScope backend
rejects `tool_choice: "required"`. Re-probed with `tool_choice: "auto"`,
**qwen3.7-max native is 2/2 byte-clean** — the native channel is healthy. No
change needed.

## Results — N=5 confirmation (pin-change candidates)

`d/f` = dispatch / fidelity out of 5.

| route | native d/f | json d/f | text d/f | verdict |
|---|---|---|---|---|
| or-kimi-k2.7-code | 5/1 | 0/0 | 5/5 | **text** — native double-escapes, json emits no Harn contract |
| di-qwen3.6 | 1/1 | 2/2 | 5/5 | **text** — native bills empty, json flaky |
| sn-minimax-m2.7 | 5/0 | 5/0 | 5/5 | **text** — native AND json corrupt backslashes |
| tog-minimax-m2.7 | 5/1 | 3/2 | 4/4 | **text** — text beats json on both dispatch and fidelity |

### Failure-mode notes

- **Backslash drift is bidirectional.** On the MiniMax-M2.7 routes the json/native
  channels do not only *double*-escape (Harmony's pathology) — they also
  *collapse* a source `\\` to `\` (the Zig multiline prefix). Either direction
  fails a byte-exact round-trip. The escape-free heredoc (`text`) channel copies
  the body verbatim and is the only reliable carrier for backslash-heavy code on
  these routes.
- **This contradicts the 2026-06-21 note** that MiniMax-M2.7's json channel
  escapes backslashes correctly. Under N=5 with this discriminator, sn-MiniMax-M2.7
  json is 0/5 clean and tog-MiniMax-M2.7 json is 2/5. The earlier read was a
  different probe; this sweep supersedes it for the fidelity axis.
- **`json` dispatch=0 on Kimi/DeepSeek-on-Baseten** means those models do not
  emit Harn's ` ```tool ` `{name,args}` contract from a plain system prompt —
  they emit provider-native `tool_calls` instead. That is a property of the
  fenced-JSON *text* channel, not the native channel (which works for those).

## Confident catalog changes (this PR)

Only routes with a decisive N=5 verdict AND no prior fidelity-grounded pin are
changed. All are MiniMax/Kimi/Qwen (in scope — not gpt-oss/GLM).

| source fragment | rule | old | new |
|---|---|---|---|
| 40-deepseek-reasoning.toml | openrouter `moonshotai/kimi-k2.7-code` (new explicit row) | native (inherited) | `text`, `tool_mode_parity = "native_unreliable"` |
| 36-deepinfra.toml | deepinfra `*qwen3.6*` | native | `text`, `tool_mode_parity = "native_unreliable"` |
| 37-sambanova.toml | sambanova `*minimax*` | native | `text`, `tool_mode_parity = "native_unreliable"` |
| 60-together.toml | together `minimaxai/minimax-m2.7*` | json / native_unreliable | `text` / native_unreliable |

Rationale: on each, the provider-native channel dispatches but corrupts
backslash-heavy bodies, and the heredoc `text` channel carries them byte-clean at
5/5. `native_unreliable` is the correct parity verdict (native cannot be trusted
for code authoring); `preferred_tool_format = "text"` steers the gate to the
escape-free channel.

## Avoid list

- **MiniMax-M2.7 on `native` or `json`** (Together, SambaNova): corrupts
  backslash-heavy file bodies (collapses `\\`, double-escapes embedded JSON). Use
  `text`.
- **Kimi-K2.7-Code on `native` (OpenRouter)**: double-escapes; **on `json`**: no
  parseable Harn contract. Use `text`. (Note: Kimi-K2.7-Code on **DeepInfra** and
  **Baseten** native IS byte-clean — provider-specific.)
- **Qwen3.6-35B-A3B on `native`/`json` (DeepInfra)**: native bills empty, json
  flaky. Use `text`.
- **StepFun step-3.7-flash (OpenRouter)**: all three channels weak in a
  single-call probe (native empty, json/text mostly unparseable). LEFT UNCHANGED —
  needs a dedicated investigation (existing pin: native/interchangeable).

## Left UNCHANGED (insufficient or confounded evidence — TODO)

- **nim-nemotron-nano / nim-nemotron-super (NVIDIA NIM)**: native returned
  `markup_no_toolcalls` and text won, BUT the existing `interchangeable` pin is
  grounded in agent-loop smoke with **reasoning OFF**, while this probe ran
  reasoning ON. Nemotron (like Harmony) may emit tool calls inside the reasoning
  channel. Methodology mismatch — do not overturn an agent-loop verdict from a
  single-call probe with different reasoning settings. TODO: re-probe with
  `reasoning` disabled before changing.
- **minimax-m3 (direct MiniMax API)**: native split (`markup_no_toolcalls` once),
  N=2 only. TODO: N=5 with reasoning handling.
- **or-gemma4-31b / tog-gemma4-31b**: native fidelity drift but json/text vary by
  host; N=2 split. TODO: N=5.
- **or-kat-coder-pro-v2**: native 2/1 (one drift), json/text clean — borderline,
  N=2 only. TODO: N=5 before flipping off native.
- **or-qwen3.7-max / -plus**: native healthy once `tool_choice: auto` is used.
  No change.
- **DeepSeek-V4-Pro (Together/DeepInfra/Baseten/NVIDIA), DeepSeek-V3.2
  (OpenRouter/SambaNova)**: native clean across the board. No change.

## gpt-oss / GLM findings (for Harn-FootgunGate, NOT edited here)

Not probed in depth this round (out of scope). The existing gpt-oss `text` pins
(Fireworks) and GLM-5.x `text`/`json` pins were left untouched.
