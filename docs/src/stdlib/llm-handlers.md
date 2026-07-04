# Composable callers and middleware

Harn's `agent_loop` and `llm_call` historically exposed only a flat
options dict for retry / fallback / shadow / budget behavior. v0.8
opens an explicit **caller seam**: `agent_loop` accepts an
`llm_caller:` closure that owns the single `llm_call(...)` invocation,
and the `std/llm/*` modules ship composable middleware for retry,
fallback, shadowing, prompt rewriting, logging, budgeting, caching,
circuit breaking, ensembles (best-of-N, self-consistency, debate),
prompt refinement, model-aware default packs, and structured-output
safety.

Concretely:

- `default_llm_caller()` from `std/llm/handlers` is the bottom of the
  middleware stack. It mirrors `agent_loop`'s built-in invocation.
- `with_*` wrappers in `std/llm/handlers` take a `next` caller and
  return a new caller. Compose them left-to-right with `compose([...])`.
- Multi-call quality strategies (best-of-N, self-consistency, debate)
  live in `std/llm/ensemble`.
- Catalog-aware option packs (`pack_for`, `pack_chat`, `pack_agent`,
  …) live in `std/llm/defaults`.
- Token / context heuristics live in `std/llm/budget`.
- Envelope-shaped consolidations (`safe_call`, `safe_field`, …) live
  in `std/llm/safe`.
- System-prompt builders live in `std/llm/prompts`.
- Catalog accessors (`model_info`, `resolved_options`, `family_of`,
  `lineage_of`, `complementary_reviewer`, `has_capability`) live in
  `std/llm/catalog`.

## Caller contract

A caller is a closure with the canonical shape:

```harn,ignore
fn(call) -> LlmCallerResult
//   call = {prompt: string, system: string|nil, opts: dict,
//           turn:  {iteration: int, session_id: string, attempt: int}}
//   LlmCallerResult =
//     {ok: true,  value: <llm dict>}
//   | {ok: false, status: string, error?: any, retryable?: bool}
```

Reserved statuses:

`"budget_exhausted"`, `"transport_error"`, `"caller_aborted"`,
`"caller_skipped"`, `"exception"`, `"schema_validation"`,
`"rate_limited"`, `"timeout"`, `"network"`, `"provider_5xx"`,
`"stream_interrupt"`, `"context_window_exceeded"`, `"auth"`,
`"policy_blocked"`, `"circuit_open"`.

Anything else lands in `error` and is preserved but not interpreted.
Wrappers must catch raw thrown errors and re-emit them as
`{ok: false, status: "exception", error: <raw>}` so middleware
composition stays total — only the bottom (`default_llm_caller`) does
the throw → envelope translation.

## Gotchas

These bit the implementing agents and will bite users:

1. **`with_retry` does not mutate `call.turn.attempt` between retries.**
   The original `call` dict is passed through unchanged. If you need
   per-attempt counting inside a custom caller, use an `atomic`. This
   is deliberate (Harn closures capture by value).
2. **`compose` takes a single list, not varargs.**
   `compose([with_logging({}), with_retry({})])(base)`, not
   `compose(a, b, c)(base)`.
3. **`text: "DONE"` is a protocol-stripping marker** in synthetic test
   callers. The runtime strips it from `visible_text`. Use `"all-ok"`
   or another string in user-visible synthetic text.
4. **`with_budget` short-circuiting only manifests in `agent_loop`
   when `loop_until_done: true`.** Otherwise the loop terminates
   naturally on iteration 1.
5. **`refine_prompt`'s session cache is best-effort.** Harn does not
   support out-of-closure mutation, so threading a mutable session
   dict requires the caller to do the threading.
6. **Streaming is out of scope for `llm_caller`.** `llm_stream_call`
   keeps its own surface; a future `llm_streaming_caller` may parallel
   this.
7. **Off-by-one in retry semantics.** The removed `llm_retries: 3`
   historically meant 4 total attempts; `with_retry`'s `max_attempts: N`
   means N total attempts. Migrations adjusting `llm_retries: K` should
   pass `max_attempts: K + 1`.

---

## `std/llm/handlers` — composable middleware

Higher-order functions returning a caller. Each `(next, opts)`
middleware (`with_retry`, `with_logging`, `with_budget`,
`with_circuit_breaker`, `with_repair`, `with_coerce`, `with_timeout`)
supports two interchangeable call shapes:

```harn,ignore
with_retry(next, opts)   // direct: returns a caller
with_retry(opts)         // curried: returns fn(next) -> caller, drops into compose
```

Compose with `compose([with_logging({...}), with_retry({...})])(base)`
or wrap explicitly with `with_retry(default_llm_caller(), {...})`.
Telemetry goes via `agent_emit_event` when `call.turn.session_id` is
non-empty.

| Function | Signature | Description |
|---|---|---|
| `default_llm_caller()` | `() -> caller` | Bottom of the stack; mirrors `agent_loop`'s built-in `__default_invoke_llm`. Returns `{ok: true, value}` on success, `{ok: false, status: "budget_exhausted"}` on budget, `{ok: false, status: "exception", error}` otherwise. Never throws. |
| `with_retry(next, opts?)` | `(caller, dict?) -> caller` | Bounded retry. Defaults: `max_attempts: 3, base_ms: 250, max_ms: 8000, backoff: "exponential", jitter: "full", honor_retry_after: true`. Honors `error.retry_after_ms` and case-insensitive `Retry-After`. Default predicate retries `transient/rate_limited/timeout/exception/network/provider_5xx/stream_interrupt`; never retries `schema_validation/auth/budget_exhausted/context_window_exceeded/policy_blocked/caller_aborted/caller_skipped/circuit_open`. Returns the last envelope plus `retries_attempted: N`. Never throws. |
| `with_fallback(callers)` | `(list<caller>) -> caller` | Try callers in order; advance on `{ok: false}`. On success: result + `{fallback_index, fallback_total}`. Emits `llm_fallback_attempt` per attempt. |
| `with_shadow(primary, shadow, opts?)` | `(caller, caller, dict?) -> caller` | Run both via `parallel each`; return primary. `sampler(call) -> bool`, `on_diff(p, s) -> nil`, `diff_when ∈ {"any","ok_only"}`. Emits `llm_shadow_diff` when text differs. |
| `with_prompt_rewrite(next, rewriter)` | `(caller, fn(prompt, system, opts) -> {prompt?, system?, opts?}) -> caller` | Rewrite the call before delegating; missing keys fall back to original. Used by `refine_caller`. |
| `with_logging(next, opts?)` | `(caller, dict?) -> caller` | Structured `llm_call_log` event per call (`latency_ms`, `model`, `provider`, `status`, `iteration`, `attempt`). `level ∈ {"debug","info","warn"}`. `include_prompt: false` by default (PII-safe). Optional `sink(record) -> nil`. |
| `with_budget(next, opts?)` | `(caller, dict?) -> caller` | Per-instance accumulator for `max_total_tokens` / `max_input_tokens` / `max_output_tokens` / `max_calls`. Counters are `atomic` so they survive Harn's by-value closure capture. `on_exceed ∈ {"throw","short_circuit"}` (default `"short_circuit"` → `{ok: false, status: "budget_exhausted"}`). Cost accounting is silently skipped when `pricing_per_1k_for(...)` is unavailable. |
| `with_cache(next, opts?)` | `(caller, dict?) -> caller` | Response memoization backed by `std/cache`. Defaults to sqlite namespace `llm.with_cache`, TTL 10 minutes, LRU size 256, and skips calls with `opts.tools` unless `skip_when` overrides that policy. Also supports the direct `with_cache(prompt, system?, options?)` convenience form. |
| `with_circuit_breaker(next, opts?)` | `(caller, dict?) -> caller` | Thin wrapper over `std/async` circuit primitives. Defaults derive the circuit name from provider/model; pass `opts.name` to share one circuit across calls. Throws the standardized `circuit_open` error when open. |
| `with_repair(next, opts?)` | `(caller, dict?) -> caller` | One-shot repair pass on `schema_validation` failures. Appends a corrective nudge (deterministic by default; override via `opts.strategy: string \| closure`) and re-asks `next` once with `max_tokens: 600` and `temperature: 0.0`. Tags the second envelope `repair_attempted: true`. Other statuses pass through unchanged. |
| `with_coerce(next, opts?)` | `(caller, dict?) -> caller` | Normalize successful envelopes for downstream consumers. Recursively lowercases keys on `value.data` (`opts.lower_keys`, default true) so callers can read fields case-insensitively without per-site `dict_get_ci` dances. Optional `opts.on_text_json: true` parses JSON-shaped `value.text` into `value.data`. Failure envelopes pass through. |
| `with_timeout(next, opts)` | `(caller, dict\|int) -> caller` | Soft, clock-aware deadline. Forwards `opts.ms` (or `opts.seconds`) to `call.opts.timeout_ms` so providers can cancel mid-flight, then post-checks elapsed time via `now_ms()`. Successes that overran convert to `{ok: false, status: "timeout", error: {timeout_ms, elapsed_ms}}`; slow failures relabel to `timeout` (set `opts.relabel_failures: false` to keep the original status). Honors the unified clock — mockable in tests. |
| `with_routing(opts)` | `(dict) -> caller` | Pre-call routing: pick a caller before the request goes out. Required `opts.default`; optional `opts.routes: list of {when: closure(call) -> bool, caller, name?}`. First matching route wins; emits `llm_routing_decision` so receipts can audit cheap-vs-frontier escalation per call. Differs from `with_fallback`, which is post-failure. |
| `compose(wrappers)` | `(list<fn(caller) -> caller>) -> fn(caller) -> caller` | Right-to-left application: `compose([a, b, c])(base) == a(b(c(base)))`. Equivalently, the leftmost wrapper is the outermost. |

### Minimal example

```harn,ignore
import {default_llm_caller, with_retry, with_logging, compose} from "std/llm/handlers"

let caller = compose([
  with_logging({level: "info"}),
  with_retry({max_attempts: 4, backoff: "exponential"}),
])(default_llm_caller())

let result = agent_loop(task, system, {
  loop_until_done: true,
  llm_caller: caller,
})
```

### Model-option resolution

For role/env model resolution, use `agent_model_options` from
`std/agent/options` (the pre-0.10 `std/agent/stack` bundle was removed;
compose retry/logging/budget middleware explicitly with the handlers above and
tool middleware from `std/llm/tool_middleware`):

```harn,ignore
import {agent_model_options} from "std/agent/options"
import {compose, default_llm_caller, with_logging, with_retry} from "std/llm/handlers"

let route = agent_model_options({
  role: "planner",
  defaults: {provider: "anthropic", model: "claude-sonnet-5", task: "agent"},
})
let caller = compose([with_retry({max_attempts: 3}), with_logging({})])(default_llm_caller())
let result = agent_loop(task, system, route.options + {loop_until_done: true, llm_caller: caller})
```

`agent_model_options(config?)` resolves explicit options first, then role
environment prefixes (`HARN_AGENT_PLANNER_*`, `HARN_LLM_PLANNER_*`,
`HARN_PLANNER_*`), then shared `HARN_AGENT_*` / `HARN_LLM_*`, then defaults. If
a model is present it calls `pack_for(...)`, resolves `tool_format: "auto"`,
and strips unsupported provider-specific keys such as `reasoning_effort` or
prompt-cache hints before the call reaches a provider
(`agent_sanitize_model_options` exposes the stripping step directly).

### Persona-shaped example: cost moat substrate

The full handler stack is the **cost moat substrate** for the
[Opinionated Harn Stack](../personas.md): cheap-model-by-default with
frontier escalation only on ambiguity, deterministic budget enforcement
per persona, and receipt-grade structured logs for every model call.

```harn,ignore
import {
  default_llm_caller, with_retry, with_logging, with_budget,
  with_routing, with_fallback, with_circuit_breaker, compose,
} from "std/llm/handlers"

// Cheap default: a fast / inexpensive model on a tight retry budget.
let cheap = with_circuit_breaker(
  with_retry(default_llm_caller(), {max_attempts: 2}),
  {threshold: 5, reset_ms: 30000},
)

// Frontier escalation: a stronger model with longer retries + a fallback
// to a second strong model if the first provider trips a circuit.
let frontier = with_circuit_breaker(
  with_fallback([
    with_retry(default_llm_caller(), {max_attempts: 3}),
    with_retry(default_llm_caller(), {max_attempts: 2}),
  ]),
  {threshold: 5, reset_ms: 30000},
)

let receipts_sink = { record ->
  // Forward to cloud receipts / IDE-host transcript / etc.
  agent_emit_event("ops.receipts", "llm_call_log", record)
}

// with_routing is a base caller (it owns the call, not a wrapper around
// `next`); the budget + logging middleware compose over it.
let router = with_routing({
  default: cheap,
  routes: [
    {name: "frontier",
     when: { call -> call?.opts?.task_kind == "judge" || (call?.opts?.escalate ?? false) },
     caller: frontier},
  ],
})

let persona_caller = compose([
  with_logging({level: "info", sink: receipts_sink}),
  with_budget({max_total_tokens: 250000, max_calls: 200}),
])(router)

agent_loop(task, system, {
  loop_until_done: true,
  llm_caller: persona_caller,
})
```

`with_routing` chooses cheap-vs-frontier **before** the request goes
out (so cost stays predictable); `with_budget` enforces the persona's
USD/token cap deterministically; `with_logging`'s `sink` emits receipt
records consumed by a cloud platform's ops console.

### First-class `routing_policy` (recommended)

The handler composition above is the right tool when each route needs
arbitrary closure-based custom logic. For the much more common
"failover chain plus per-call budget" case, build a `routing_policy`
once and pass it to `llm_call(... routing: policy ...)` directly:

```harn,ignore
let policy = routing_policy({
  chain: [
    {provider: "anthropic", model: "claude-opus-4-20250514"},
    {provider: "openai",    model: "gpt-4o"},
    {provider: "ollama",    model: "llama4:70b"},      // local fallback
  ],
  failover: {
    on_status: [429, 500, 502, 503, 504],
    on_timeout_ms: 30_000,
    on_error_kinds: ["rate_limit", "schema_validation"],
    max_attempts: 3,
  },
  latency: {race_after_ms: 5000},
  budget:  {per_call_usd: 0.5, on_exceed: "abort"},
  observe: {emit_event: "billing.routing_decision"},
  escalate_on: [                                       // optional verifier chain
    {kind: "typecheck"},
    {kind: "lint", forbidden_patterns: ["TODO"], on_fail: "refine"},
  ],
})

let result = llm_call("Summarize this PR.", nil, {routing: policy})
```

`escalate_on` makes frontier escalation **conditional on a
code-quality signal** rather than static routing. Verifiers see the
candidate's text after a successful link: `accept` keeps it,
`refine` retries the same link with the verifier's reason appended to
the prompt (capped by `max_refines_per_link`, default `1`),
`escalate` advances to the next link. Three built-in kinds —
`typecheck` (harn-parser), `lint` (regex `forbidden_patterns` /
`required_patterns` / `max_line_length`), and `test_run` (spawns a
configurable command with the candidate on stdin) — let scripts gate
"only call Opus when Devstral's answer doesn't typecheck." See the
quickref for the full per-kind option list.

Compared to `compose([with_routing, with_retry, with_fallback])`, the
primitive is replay-deterministic (every attempt rides on the result
envelope's `routing` block), records its own tape events
(`<dispatch>.{decision,attempt,race_started,race_won,race_lost,budget_exceeded,verifier_signal,exhausted}`),
and pays for `latency.race_after_ms` racing out-of-the-box. Migrate
existing chains by replacing the wrapper composition with one
`routing_policy({...})` call; layer `compose([with_logging,
with_budget, ...])` over the policy only when you need bespoke
closure-level instrumentation.

### Error / envelope semantics

Every wrapper returns the same envelope shape:

- `{ok: true, value: <llm dict>}` on success
- `{ok: false, status: <reserved string>, error?: any, retryable?: bool}`
  on failure

Wrappers must not throw. `with_retry` and `with_fallback` decorate
their results with `retries_attempted` / `fallback_index` so callers
can observe what happened. The `agent_loop` runtime validates the
shape at the seam and emits a friendly diagnostic if a custom caller
returns a non-dict.

### Composition story

The `agent_loop` seam (`opts.llm_caller`) accepts any caller. For
direct `llm_call`-style usage, callers can invoke their wrapped caller
explicitly:

```harn,ignore
let caller = with_retry(default_llm_caller(), {max_attempts: 3})
let envelope = caller({
  prompt: "hello",
  system: nil,
  opts: {provider: "auto", model: "claude-sonnet-5"},
  turn: {iteration: 0, session_id: "", attempt: 1},
})
if envelope.ok { log(envelope.value.text) }
```

`std/async.retry_with_backoff` is **not** the same surface — it
operates on arbitrary closures with a predicate, not on the caller
seam. Use `with_retry` for `llm_caller` middleware.

---

## `std/llm/ensemble` — multi-call quality strategies

| Function | Signature | Description |
|---|---|---|
| `best_of_n(prompt, system, opts?)` | `(string, string\|nil, dict?) -> dict` | Sample `n` candidates (default 5; clamped to `[2, 32]`) at high temperature, then ask a judge to pick the best. `judge ∈ "structured" \| closure`. Optional `reward(text) -> float` pre-filter. Returns `{ok, best: {text, index}, candidates, judge, reasoning}`. All-fail → `{ok: false, status: "all_samples_failed"}`. |
| `self_consistency(prompt, system, opts?)` | `(string, string\|nil, dict?) -> dict` | Sample `n` (default 8) at temperature 1.2, extract a canonical answer per sample with `extract(text)` (required), majority vote. `vote ∈ {"majority", "weighted"}`; weighted requires `confidence_fn`. Returns `{ok, answer, answer_count, total, distribution, candidates, entropy}`. Ties: lowest-index wins, emits `self_consistency_tie`. |
| `parallel_judge(items, judge_fn, opts?)` | `(list, fn(item) -> verdict, dict?) -> list<dict>` | Run `judge_fn` over `items` with bounded concurrency (`max_concurrent`, default 4). Output preserves input order. `on_error ∈ {"skip", "fail_fast", "collect"}`. Each entry: `{item, verdict?, ok, error?, duration_ms, index}`. |
| `debate(opts)` | `(dict) -> dict` | Multi-agent debate. Required: `opts.prompt`, `opts.debaters` (≥ 2), `opts.judge`. Defaults: `n_rounds: 2`, `parallel_within_round: true`, `sampler_opts: {temperature: 0.7}`. Returns `{ok, winner, rounds, judge, transcript}`. |

Citations (in source):

- Self-consistency: Wang et al. 2022, [arxiv:2203.11171](https://arxiv.org/abs/2203.11171).
- Multi-agent debate: Du et al. 2023, [arxiv:2305.14325](https://arxiv.org/abs/2305.14325).

### Minimal example

```harn,ignore
import {best_of_n} from "std/llm/ensemble"

let result = best_of_n(
  "Write a haiku about debugging.",
  "You are a poet.",
  {n: 5, sampler_opts: {temperature: 1.0}},
)
log(result.best.text)
log(result.reasoning)
```

### Composition with `agent_loop`

`best_of_n` returns a result dict, not a caller. To use ensemble
sampling inside `agent_loop`, wrap it in a caller:

```harn,ignore
let ensemble_caller = { call ->
  let r = best_of_n(call.prompt, call.system, {n: 3} + call.opts)
  if !r.ok { return {ok: false, status: r.status} }
  return {ok: true, value: {text: r.best.text}}
}

agent_loop(task, system, {llm_caller: ensemble_caller})
```

Ensemble functions emit `ensemble_cost` events with estimated
token-fanout cost. Wrap in `with_budget` to cap aggregate spend.

---

## `std/llm/refine` — meta-prompt prompt rewriting

| Function | Signature | Description |
|---|---|---|
| `refine_prompt(opts)` | `(dict) -> dict` | One-shot meta-prompt rewrite of `opts.user_prompt`. Optional `model`, `provider`, `session`, `target_size ∈ {"auto","small","medium","large"}`, `style ∈ {"imperative","concise","structured","chain_of_draft"}`, `keep`, `strip`, `meta_prompt`. Returns `{ok, refined, original, diff_summary, est_tokens_before, est_tokens_after, style, target_size, model}`. |
| `refine_caller(next, refine_opts?)` | `(caller, dict?) -> caller` | Wraps `next` so the prompt is refined once and threaded through every subsequent call. Composes naturally with `compose`. |

`target_size: "auto"` heuristic over `estimate_text_tokens(user_prompt, model)`:

- `<= 60` → `"small"`
- `<= 1200` → `"medium"`
- otherwise → `"large"`

The refiner asks the model to emit a single `DIFF: <summary>` trailer
which is parsed off the response. Citations: DSPy MIPROv2, OpenAI
Prompt Optimizer guide, OpenAI Cookbook meta-prompting recipe.

### Example

```harn,ignore
import {refine_prompt} from "std/llm/refine"

let r = refine_prompt({
  user_prompt: "summarize this report",
  style: "imperative",
  target_size: "small",
  keep: ["MUST cite section numbers"],
})
log(r.refined)
log(r.diff_summary)
```

The session cache is best-effort: pass `opts.session = {...}` and
re-pass the same dict on subsequent calls to short-circuit identical
refinements.

---

## `std/llm/budget` — token and context heuristics

| Function | Signature | Description |
|---|---|---|
| `estimate_text_tokens(text, model)` | `(string, string) -> int` | Heuristic: English `len/4`, code-like `len/3.5`, CJK-heavy `len*1.0`. The `model` arg is reserved for a future Rust tokenizer builtin. Note: not named `estimate_tokens` to avoid collision with the workflow builtin of the same name. |
| `context_window_for(model)` | `(string) -> int` | Looks up `llm_model_info(model).catalog.context_window`. Falls back to `8192`. |
| `recommend_max_output_tokens(opts)` | `(dict) -> int` | `ctx − used − ceil(ctx*headroom)`, then task-clamped. Required: `opts.prompt`, `opts.model`. Optional: `system`, `headroom` (0.10), `task_kind` (`"chat"`/`"agent"`/`"plan"`/`"code"`/`"json"`/`"summarize"`), `summary_ratio` (0.30). Floor `64`. |
| `budget_summary(opts)` | `(dict) -> dict` | Debug helper returning all intermediate values plus an `assumptions` list. |
| `fits_in_context(text, model, headroom?)` | `(string, string, float?) -> bool` | Quick boolean check after reserving `headroom` of the window. Default `headroom: 0.10`. |

### Example

```harn,ignore
import {recommend_max_output_tokens, fits_in_context} from "std/llm/budget"

let max_out = recommend_max_output_tokens({
  prompt: long_text,
  system: sys,
  model: "claude-sonnet-5",
  task_kind: "summarize",
})

if !fits_in_context(long_text, "gpt-4o") {
  // compress / summarize first
}
```

---

## `std/llm/defaults` — model-aware option packs

`pack_for(opts)` returns a complete `llm_call`-ready options dict,
calibrated for the model's provider/family and pinned to a task. User
opts always win.

Layering (low → high):

1. `resolved_options(opts)` — runtime catalog defaults
2. effort patch (per family)
3. thinking patch (per family; explicit caller intent wins)
4. task overlay (only fills unset fields)
5. `recommend_max_output_tokens(...)` if a prompt was provided and
   max_tokens hasn't been set yet
6. user opts — highest precedence

| Function | Signature | Description |
|---|---|---|
| `pack_for(opts)` | `(dict) -> dict` | Required: `opts.model`. Optional: `provider`, `task ∈ {"chat","agent","refine","judge","summarize","code","json"}` (default `"chat"`), `thinking ∈ {"off","low","medium","high","auto"}` (default `"auto"`), `effort ∈ {"fast","balanced","quality","auto"}` (default `"balanced"`), plus any other `llm_call` keys. |
| `llm_apply_reasoning_policy(opts)` | `(dict) -> dict` | Applies Harn's provider-aware `reasoning_policy` / `thinking_policy` abstraction to an option dict. Used by `agent_loop`; direct callers can use it before `llm_call` when they want the same calibration. Explicit `thinking` and `reasoning_effort` win. |
| `pack_chat(model, opts?)` | `(string, dict?) -> dict` | Convenience wrapper for `task: "chat"`. |
| `pack_agent(model, opts?)` | `(string, dict?) -> dict` | `task: "agent"`. |
| `pack_refine(model, opts?)` | `(string, dict?) -> dict` | `task: "refine"`. |
| `pack_judge(model, opts?)` | `(string, dict?) -> dict` | `task: "judge"` (sets `output_format: {kind: "json_schema"}`, `temperature: 0.0`, `schema_retries: 2`). |
| `pack_summarize(model, opts?)` | `(string, dict?) -> dict` | `task: "summarize"`. |
| `pack_code(model, opts?)` | `(string, dict?) -> dict` | `task: "code"`. |
| `pack_json(model, opts?)` | `(string, dict?) -> dict` | `task: "json"` (sets `output_format: {kind: "json_object"}`). |

Calibrated families: Anthropic Sonnet/Opus/Haiku 4.x, OpenAI
GPT-5/5.5/4o/4.1, Gemini 2.5 Pro/Flash, Ollama Qwen3/Llama 3.x.

### Edge cases

- **Opus 4.7 + manual `thinking`**: stripped + warns
  `pack_thinking_stripped` (Opus 4.7 returns 400 server-side on manual
  budgets).
- **Ollama Qwen3 + `thinking: "off"`**: relies on the runtime's
  capability-driven `/no_think` injection; the pack does not duplicate.
- **`provider: "auto"` unresolvable**: minimal pack.

### Example

```harn,ignore
import {pack_agent} from "std/llm/defaults"

let opts = pack_agent("claude-sonnet-5", {
  thinking: "medium",
  effort: "quality",
})
agent_loop(task, system, opts + {loop_until_done: true})
```

---

## `std/llm/safe` — DRY consolidations

| Function | Signature | Description |
|---|---|---|
| `safe_call(prompt, system, options)` | `(string, string\|nil, dict) -> dict` | Try-wrap `llm_call` into `{ok: true, value}` or `{ok: false, status: "budget_exhausted"\|"exception", error}`. Same shape as `default_llm_caller`. |
| `safe_field(envelope, names, default)` | `(dict, list<string>, any) -> any` | Try each name (case-insensitive) in order; return first non-nil non-empty value, else default. Top-level keys only. |
| `dict_get_ci(d, key)` | `(dict, string) -> any` | Single-key case-insensitive lookup. |
| `with_case_insensitive_keys(envelope)` | `(any) -> any` | Recursively lowercase all dict keys. Idempotent. |
| `structured_envelope_or_default(envelope, defaults)` | `(dict, dict) -> dict` | Merge `defaults` under `envelope.data`; envelope wins per-key. Bails on `ok: false` or non-dict. |
| `judge_payload(session, opts, stop_reason, text, iteration)` | `(dict, dict, string, string, int) -> dict` | Re-export of `agent/judge.__judge_payload` shape construction. |
| `verdict_normalize(text, alias_groups)` | `(string, list<{canonical, aliases}>) -> string` | Lowercase, trim, optionally map via alias groups. |
| `schema_retry_nudge_for(schema, hint?)` | `(dict, string?) -> string` | Auto-generate corrective nudge from a schema's required fields. |

### Example

```harn,ignore
import {safe_call, safe_field, with_case_insensitive_keys} from "std/llm/safe"

let r = safe_call(prompt, system, {provider: "auto", model: "gpt-4o"})
if !r.ok { return r }

let envelope = with_case_insensitive_keys(parse_json(r.value.text))
let verdict = safe_field(envelope, ["verdict", "decision", "result"], "unknown")
```

---

## `std/llm/prompts` — system-prompt builders

| Function | Signature | Description |
|---|---|---|
| `system_prelude(opts)` | `(dict) -> string` | Build a structured system prompt from `persona` (required), `tools`, `constraints`, `output_contract`, `examples`, `tone ∈ {"professional","terse","conversational"}`. Deterministic / cache-friendly. |
| `tool_use_prelude(tools, format)` | `(list<string>, string) -> string` | Render a tool-use prelude. `format ∈ {"native", "text"}`. |
| `structured_output_preface(schema, opts?)` | `(dict, dict?) -> string` | Render a JSON-schema preface from `schema.required` + `schema.properties` (sorted). Pass `opts.template` to use a custom prompt asset. |

### Example

```harn,ignore
import {system_prelude} from "std/llm/prompts"

let sys = system_prelude({
  persona: "You are a release auditor.",
  tone: "terse",
  constraints: ["Cite evidence by file path", "No speculation"],
  output_contract: {format: "json", required: ["risks", "recommendation"]},
})
```

---

## `std/llm/catalog` — Harn-side catalog accessors

Thin wrappers over the `llm_resolved_options` / `llm_model_info` Rust
builtins. The Harn-side names are deliberately shorter and don't
shadow the builtins.

| Function | Signature | Description |
|---|---|---|
| `model_info(selector)` | `(string) -> dict` | Wraps `llm_model_info`. Always returns a dict; `catalog` field is nil for unknown models. |
| `resolved_options(opts)` | `(dict) -> dict` | Wraps `llm_resolved_options`. Required: `opts.model`. |
| `has_capability(model, capability)` | `(string, string) -> bool` | Capability ∈ `{"thinking", "tool_search", "interleaved_thinking", "prompt_caching", "vision", "audio", "pdf", "files_api", "reasoning_effort", "native_tools"}`. |
| `family_of(model_id)` | `(string) -> string` | Returns the normalized review-diversity family such as `"anthropic-claude"`, `"openai-gpt"`, `"google-gemini"`, or `"qwen"`. Hosted aliases keep the underlying model family. |
| `lineage_of(model_id)` | `(string) -> string` | Returns the narrower calibration lineage such as `"claude-opus-adaptive"`, `"openai-gpt5"`, `"gemini-flash"`, or `"qwen3"`. Drives `pack_for` defaults. |
| `complementary_reviewer(opts)` | `(dict) -> dict` | Wraps `llm_complementary_reviewer`. Required: `opts.author_model`; optional: `author_provider`, `intent`, `max_price_multiplier`. |

### Example

```harn,ignore
import {complementary_reviewer, family_of, has_capability, lineage_of} from "std/llm/catalog"

if has_capability(model, "thinking") {
  // safe to set `thinking: "medium"` in opts
}

let fam = family_of(model)       // e.g. "anthropic-claude"
let lineage = lineage_of(model)  // e.g. "claude-opus-adaptive"
let reviewer = complementary_reviewer({author_model: model, intent: "plan_review"})
```

---

## See also

- [LLM calls](../llm/llm_call.md) — `llm_call`, `llm_call_structured`, `llm_call_safe`.
- [Agent loops](../llm/agent_loop.md) — `agent_loop` and the `llm_caller:` seam.
- [LLM quick reference](../docs/llm/harn-quickref.md) — the one-page cheat sheet.
