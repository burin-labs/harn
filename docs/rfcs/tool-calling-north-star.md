# RFC: Tool-calling north star — Harn owns the dialect, forever

Status: Draft
Author: Harn runtime
Audience: Harn maintainers; IDE hosts, cloud platforms, harn-bump-fleet, 20eq consumers
Scope: the tool-calling stack in `crates/harn-vm/src/llm/**`

## 0. One-sentence thesis

Harn already holds every fact and almost every mechanism needed to make
tool-call dialect mismatches structurally impossible; the remaining work is to
(1) close the silent-`Ok` and not-yet-fingerprinted thrown failure modes into the
same reactive self-heal loop that already exists for #3500, ordered as a remedy
*ladder* (continue-on-truncation → bounded retry → channel-switch as a one-way
last resort) and topped with a feedback-enrichment tier below escalation; (2)
turn the per-route capability registry into a *living* one that proposes its own
updates from observed health; and (3) keep **native structured tool-calls the
default** while offering code-mode as a **capability/tier-gated opt-in for scale**,
not a cutover. The north star is **"native-default, code-mode-for-scale"**: no
consumer should ever again pin a `tool_format`, read a provider dialect, or debug
a vanishing tool stream — and we get there by making the *static* registry + the
*reactive* self-heal the answer, not by betting the default agent loop on a
paradigm the evidence says taxes exactly our cheap lane.

## 0a. Hard design constraint — compose existing primitives, never reinvent

Every mechanism in this RFC (code-mode, self-heal, both preflight tiers, the
living registry, model-switch) is a **composition of building blocks Harn already
ships**, not a parallel system. This is a binding constraint on the
implementation, not a stylistic preference, because the failure mode we are most
exposed to is a pile of narrow one-off hacks — exactly the whack-a-mole this RFC
exists to end. Concretely, reuse:

- the **capability registry + `validate_tool_format` gate** (#3462) as the single
  source of truth and the only *binding* enforcement layer;

- the **#3500 signature-keyed degrade** (`degrade_options_to_text_channel`) as the
  recovery action — we widen its *trigger*, we do not write a second degrade;

- the deterministic, name-free **`is_billed_noncommittal_completion`** signal set
  (`response.rs:553`) as the integrity classifier's core — we do not re-derive
  "this turn vanished its call";

- the existing **continue-on-truncation** path (`agent_session_host` auto-continue
  on length truncation with a partial call) as tier 1 of the remedy ladder — we
  ratify it, we do not rebuild it;

- the **`DiagnosticEnvelope` (#2677) + edited-window renderer + dependency
  diagnostics (#2678)** as the feedback-enrichment tier's materials — we wire
  existing facts into verifier feedback, we do not invent a new diagnostic format;

- the **`mock`/`fake` provider seam + `tool_conformance` round-trip harness** for
  the optional advisory probe and every mechanism-fitness test;

- **code mode's existing `composition_*` builtins** (`docs/src/code-mode.md`) as
  the code-mode runtime — Phase 4 adds a tier gate *around* them, it does not build
  a new executor.

If any piece would balloon LOC or system complexity, the RFC's standing
instruction is: **extend the existing primitive, and say so here.** Two places
where this rule already changed the design: (1) the integrity guard reuses
`is_billed_noncommittal_completion` rather than inventing a vanishing-call
detector — the sweep confirmed the only vanishing shape *is* the billed-
noncommittal one; (2) we *rejected* a live first-run capability probe (a new
system) in favor of the static registry + reactive self-heal we already ship,
matching what every SOTA agent actually does.

## 1. Why this RFC exists

A coding agent that does nothing wrong and still goes nowhere is the most
expensive bug we ship, because it reads as a model-capability failure on the
meter when it is actually a parsing failure in the harness. The dialect problem
is real and multi-headed (see the "tool-call dialect problem" write-up
and §2 evidence): "native function calling" is at least four
mutually incompatible wire dialects across the cheap/open routes we actually use,
and the dominant failure mode is *silence* — `tool_calls: []` with a billed,
clean-finished turn.

We have responded incrementally and well: the capability registry
(`capabilities.rs`), the enforced `validate_tool_format` gate (#3462), name
canonicalization before policy (#3466/#3434), the bounded tool contract (#3469),
and the signature-keyed native→text degrade for thrown 5xx/EOF tool-parser
failures (#3500). Each closed a real footgun. But they were built one provider
crisis at a time. This RFC states the north star they were all reaching toward
and commits to the architecture that ends the whack-a-mole.

## 2. Evidence (the failure taxonomy we are designing against)

The failure signatures below are the ground truth from mining the most recent
~50 eval transcript runs (2026-06-18 → 2026-06-24) across the two live
eval roots: an IDE-host eval corpus (895 run dirs) and
`/private/tmp/bc-hardcell-harness/.burin-evals`. Counts are from the forensic
sweep; byte-level excerpts and absolute source paths are in Appendix A. The
single most important empirical finding shapes the whole design:

> **"True vanishing tool_calls" (`stop_reason == tool_calls` with an empty array)
> did NOT occur in any of the 50 recent transcripts.** Every empty/reasoning-only
> failure surfaced either as a *thrown* `provider_call_error` (DeepInfra,
> SambaNova) or as a *returned* response with `stop_reason == length`/`stop`,
> empty text + empty `tool_calls`, and a multi-thousand-token `thinking` channel.
> So the integrity guard must key on the **billed-noncommittal signal set**
> (which is *already* detected deterministically and thrown), not on a
> `tool_calls: []`-with-`tool_calls`-stop shape that does not occur in practice.

| # | Signature | Count (recent sweep) | Models / routes | Returns | Today's handling | Gap |
|---|-----------|------|-----------------|---------|------------------|-----|
| S1 | DSML text-only on a native pin (DeepSeek V3.2) | not reproduced recently (route now pinned `text`) | `openrouter/deepseek-v3.2` | text body holds `<｜DSML｜invoke …>` | `validate_tool_format` pre-steers route to `text` so DSML parser runs | Pre-empted IF registry parity correct; no runtime catch if a *new* route does this |
| S2 | **Billed-noncommittal / reasoning-channel-only** | **13** (DeepInfra) + 5 (Fireworks judge, `stop=length`) | `deepinfra/openai/gpt-oss-120b` tf=**native**; `fireworks/gpt-oss-120b` | **Throws** `billed_noncommittal_completion_error` (DeepInfra) / returns `stop=length` reasoning-only (Fireworks) | bounded empty-completion retry on the **same native channel** | **Re-fails on retry; never degrades channel** — the headline gap |
| S3 | `<tool_call>`-markup runaway in JSON judge call (GLM-5.x) | 1 | `together/zai-org/GLM-5.1`, `stop=length` | `Ok`, content holds literal `<tool_call>` repeated to token cap | `reserved_tool_call_token` remap + tagged parser | Degenerate JSON-mode repetition in a *structured-output judge* call, not the agent tool loop — distinct surface |
| S4 | JSON backslash double-escape (gpt-oss Harmony on `json`) | not reproduced (routes now `text`; minimax on `json` clean) | `gpt-oss` Harmony | tool call parses, **argument bytes corrupt** | routes pinned to `text` (#3505/#3506); minimax-m2.7 confirmed clean on `json` | Pre-empted IF registry pin correct; byte-level, invisible to a call-count check |
| S5 | Empty/dropped/truncated arguments | folded into S2/length-truncation in sweep | gpt-oss family | `Ok`/throw | `partial_tool_args.rs` best-effort repair | No channel re-resolution when repair fails |
| S6 | **Tool-protocol failure (SambaNova)** | **6** (distinct call_ids, aborted run) | `sambanova/gpt-oss-120b` tf=**native** | **Throws** HTTP 400 `Model started a function call but did not complete it` | generic retry; #3500 degrade only if 5xx/EOF fingerprint matches | **400 (not 5xx) so #3500 fingerprint misses it; never degrades** |
| S7 | Free-tier TPM 413 (Groq) | 9 (3 exhausted) | `groq/openai/gpt-oss-120b` | **Throws** 413 rate-limit | rate-limit cooldown + breaker | Orthogonal to dialect; correctly handled |
| S8 | Harmony `to=functions.NAME` / `<\|constrain\|>json` literal name | handled, none leaked | `gpt-oss` Harmony | `Ok`, tool name is a provider artifact | `recover_namespaced_name` / `normalize_tool_call_shape` before policy | Handled |
| S9 | Anthropic transcript-replay reject | 6 | `anthropic` (sonnet) | **Throws** 400 `messages.N.reasoning: Extra inputs are not permitted` | — | A stored reasoning field re-sent to the Anthropic API; a transcript-replay bug, separate line item |
| N1 | **Cerebras gpt-oss-120b native — CLEAN** | 58/65 single well-formed call, 0 errors | `cerebras/gpt-oss-120b` tf=**native** | `Ok` | — | *Negative control:* native gpt-oss is route-dependent, not universally broken — the registry asymmetry (don't auto-demote a clean native route) is correct |

The structural lessons from the table:

1. **Our proactive defenses (registry gate, canonicalization) only fire when the
   registry fact is already correct.** They pre-empt S1/S4/S8 only because the
   route pins are right today.

2. **Our reactive defense (#3500 degrade) only fires on *thrown* 5xx/EOF.** It
   misses S2 (the billed-noncommittal throw — 13 occurrences — routes to a
   same-channel retry) and S6 (SambaNova's 400 — wrong status class for the
   fingerprint). These are *the two most common hard tool-call failures in the
   sweep* and neither degrades today.

3. **Native gpt-oss is route-dependent (N1).** Cerebras serves it clean; DeepInfra
   and SambaNova do not. This validates the conservative
   propose-demotions-never-auto-promote rule for the living registry (§4c).

So the highest-leverage fix (Phase 1) is precisely: make the degrade fire on S2
and S6, not just S6's narrow 5xx/EOF subset.

## 3. Current architecture (where the seams are)

Data flow (host → VM → provider and back):

1. **Format resolution.** `llm_config::default_tool_format(model, provider)`
   resolves the channel; `capabilities::validate_tool_format` (the #3462 gate)
   auto-corrects a requested format the route can't carry, keyed on
   `tool_mode_parity` / `text_tool_wire_format_supported`, preferring
   `preferred_tool_format`. Single source of truth: `capabilities.toml`
   (generated from `capability_sources/**`).

2. **Request.** `observed_llm_call` (`agent_observe.rs:1250`) wraps one provider
   call with observability + retry. `effective_tool_format` is computed at
   `:1261`. The working request is a `Cow<LlmCallOptions>` so a mid-retry degrade
   is zero-copy until it fires.

3. **Response normalization.** `api/response.rs::parse_llm_response` turns the
   provider body into an `LlmResult`. The deterministic, name-free
   `is_billed_noncommittal_completion` (`response.rs:553`) detects S2 from
   structural signals (`CompletionContractSignals`) and **throws**
   `billed_noncommittal_completion_error` (`:567`). Text-channel dialects are
   parsed by `tools/parse/{tagged,fenced_json,bare,native_json}.rs`.

4. **Self-heal (today).** In `observed_llm_call`'s loop:
   - `Ok` arm: `is_retryable_unproductive_completion` (zero-token empty +
     errored-actionless) → bounded empty-completion retry (`:1391`).

   - `Err` arm: `is_empty_completion_retry_error` (zero-token empty +
     billed-noncommittal) → bounded retry on the **same channel** (`:1574`);
     `is_native_tool_channel_failure` (5xx/EOF + tool-parser fingerprint, native
     channel only) → `degrade_options_to_text_channel` **once**, then retry on
     text (`:1586`, `:1680`). The degrade is in-place (mutates the working
     `LlmCallOptions`, re-sends the same turn), persists nothing to the registry.

5. **Format contract.** `docs/tool-format-contracts.md`: a transcript claims one
   `tool_format` for life; a cross-format model switch (text↔native) requires a
   reset/fork/compact. `agent_session_claim_tool_format` enforces it.
   `native_tool_fallback` policy defaults to `reject`.

6. **Code mode (today).** `composition_*` builtins already exist: a read-only
   executor (`composition_execute`), a binding manifest
   (`composition_binding_manifest`), MCP profile, and a crystallization bridge.
   See `docs/src/code-mode.md`. It is positioned as exploration that feeds
   crystallization, not yet as the primary agent-loop paradigm.

The two key takeaways for the design:

- **`is_native_tool_channel_failure` is too narrow.** It is the only thing that
  triggers a channel degrade, and it only matches a thrown 5xx/EOF
  tool-parser shape. The billed-noncommittal throw (S2) — which is *the* canonical
  cheap-model vanishing-call signature and is already detected deterministically
  one layer down — routes to a same-channel retry and never degrades. This is the
  single highest-leverage, lowest-risk fix in the whole RFC.

- **The `Ok` arm has no parse-integrity check.** S3/S5 can come back as a
  parseable-but-wrong `Ok`. There is no "before we accept this turn, is the tool
  stream actually intact?" hook.

## 4. The decisions

### (a) Code mode — recommendation: NATIVE structured calls stay the DEFAULT; code-mode is a CAPABILITY/TIER-GATED opt-in for scale

**Position: native structured tool-calls remain the default agent paradigm —
especially for the cheap lane — and code-mode is an opt-in layer, gated on model
tier/capability AND on a large/deferred tool surface, never a cutover of the
default loop.** Framing: *native-default, code-mode-for-scale.*

The evidence is decisive and it points away from a code-mode default:

- **CodeAct (ICML 2024) shows the code-action advantage WIDENS with capability
  and INVERTS for weak models** — on M3ToolEval the best open model scored 13.4%
  vs GPT-4's 74.4%. Authoring correct control flow is itself a capability test,
  and our cheap lane (gpt-oss / GLM / Together MiniMax) is precisely the
  population least able to pass it. Defaulting them to code-mode would tax the
  models that most need a robust floor.

- **The headline code-mode numbers (Anthropic "Code execution with MCP",
  Cloudflare "Code Mode") are token-reduction, self-reported, and not tool-call
  *correctness*.** Token efficiency is real and valuable at scale; it is not
  evidence that code-mode raises pass@1 for weak models.

- **No frontier agent ships code-mode as its default.** Anthropic ships
  "programmatic tool calling" as an explicit opt-in *layered on* native structured
  calls; Claude Code's base loop is native. We should match that shape: native
  base loop, code-mode as a deliberate layer for large surfaces and capable
  models.

Therefore any flip of the default to code-mode for a tier is a **convergence
claim** and must clear the meter gate per `AGENTS.md`: paired 95% CI lower bound
`> 0`, N≥5, **per model tier**, before it ships as default. Until then code-mode
stays opt-in.

Where code-mode genuinely wins (and so where the opt-in pays off):

- **It collapses the dialect problem to one dialect: code.** Every provider can
  write a short program. A model that emits `read_file({path})` inside a Harn
  snippet is emitting *text the VM parses with one grammar we own*, not a
  provider-native `tool_calls` array whose serialization we don't control. S1/S2/
  S3/S5/S8 are all artifacts of the provider's tool-call *serializer*; code mode
  routes around the serializer entirely. The remaining risk (S4 backslash
  double-escape) is a function of *string-argument escaping*, and a heredoc-bearing
  code grammar (which Harn's text format already has — `name({...})` with
  `<<TAG … TAG` bodies) carries backslash-heavy code byte-clean, which the
  blog-draft probe confirmed.

- **It is context-efficient and composable.** One reviewed parent operation does
  loop/filter/sort over results instead of N round-trips of one-call-at-a-time,
  with every child binding still audited (`composition_execute` reports
  `child_calls`/`child_results`). For a 30+-tool MCP surface this is the
  difference between re-sending 12k tokens of tool docs every turn (see the token
  forensics in burin memory) and sending a compact binding manifest once.

- **It feeds crystallization.** `composition_crystallization_trace` turns a good
  snippet into durable, replay-gated workflow code. Structured tool-calls have no
  comparable promotion path. This is the long-term compounding win.

What the model sees (the tool-API surface): `composition_binding_manifest` →
`composition_harn_api` produces typed `.harn` wrappers, one function per binding,
with schemas, side-effect level, capabilities, and examples; `{form: "compact"}`
for prompt budget. Optionally `composition_typescript_declarations` emits `.d.ts`
for editor/model ergonomics, but **Harn-native execution + the binding manifest
stay the runtime authority** — TS is sugar, never the wire contract.

Sandboxing/safety: the MVP executor is read-only and fails closed — rejects
imports, subprocess, network, workspace writes, HITL, parallel/spawn, and any
call outside the manifest bindings plus a curated pure-helper list. Child calls
route through the normal dispatcher (policy, schema validation, approval hooks,
budgets, allowlists, circuit breakers all stay attached). This is a strictly
tighter instance of Anthropic "Code execution with MCP" / Cloudflare "Code Mode";
we are not inventing the sandbox concern, we are constraining it harder.

Error feedback: a binding call that fails raises a typed runtime error inside the
snippet with a failure category; the report carries per-child status. A snippet
that references an unknown binding fails closed with the allowed binding set
named (same teaching-denial discipline as the #3469 bounded contract).

**Why code-mode must NOT be the default (the decisive counter-case):**

- **Weak/non-coding models degrade badly.** A model that can't reliably write a
  for-loop will write a broken snippet, and a snippet parse/exec failure is a
  *worse* failure than a single bad tool call — it wastes the whole turn. This is
  CodeAct's inversion in operational form. The cheap-model meter is exactly the
  population we care most about, and it is exactly the population least able to
  author correct control flow.

- **Single fixed-shape operations are better as one tool call.** A bundle or
  batched read tool beats a snippet when the operation is known and fixed — less
  to parse, less to go wrong. `docs/src/code-mode.md` already says this; the RFC
  ratifies it.

- **Latency floor.** Authoring + executing a snippet has higher fixed cost than a
  single call for trivial one-step tasks.

**Two gates on code-mode (both required to enter the opt-in layer):**

1. **Model-tier / capability gate** — a `code_mode_capable` registry fact, initial
   value derived from `strengths` / coding capability (frontier-tier on by
   default; cheap lane off), refined from observed snippet exec-success rate via
   the §4c drift detector. This is the gate the cheap-lane evidence demands.

2. **Side-effect / approval gate** — code-mode's existing read-only executor +
   AutoApprove posture already gates it; we keep that and add the tier gate
   *above* it.

**The paradigm ladder (native is the DEFAULT rung):**
`structured native tool-calls` (the DEFAULT for every capable native route,
including the whole cheap lane) → `text/json tool-calls` (the dialect-safe floor
the gate guarantees when native is broken for the route). **Code-mode is a
sideways opt-in layer above the native default for capable models on a large/
deferred surface — not a rung the cheap lane is pushed up onto.** A capable model
in the code-mode layer that fails snippet authoring N times in a session
auto-demotes back to the native default for the rest of the session.

### (b) A runtime self-heal layer that works regardless of paradigm — recommendation: BUILD IT, by generalizing #3500 into a closed loop

This is the must-ship runtime layer. Three pieces.

**The preflight ANSWER is the static living registry + reactive self-heal — NOT a
live capability probe.** A SOTA scan (Aider, Cline, OpenCode, Zed, Codex,
OpenHands, vLLM, llama.cpp) found that *nobody* runs a live tool-calling
capability probe at session start. The universal pattern is exactly the two
halves Harn already has:

- a **static per-`(provider, model)` safe-format table** (Aider benchmarks once →
  a defaults table; OpenHands `FUNCTION_CALLING_SUPPORTED_MODELS`; llama.cpp
  template detection) — this is our **capability registry + `validate_tool_format`
  gate** plus the GLM/Moonshot alias pinning, made *living* by §4c; and

- a **reactive, signature-based fallback at runtime** — this is the **#3500
  signature-keyed degrade**, generalized by (b2)/(b3) below.

A live first-run probe was considered and **rejected as the primary mechanism**:
it adds first-task latency, it courts false confidence (vLLM's own docs note a
route can pass a probe and still fail the real task), and a *repo* dimension adds
**zero** format signal — tool-call format is a `(provider, model)` property, not a
repo property. So the static table + reactive self-heal carry the load.

**Optional advisory channel micro-probe (b1) — NOT a gate.** If we keep any probe
at all, it is strictly **advisory and `(provider, model)`-keyed** (never repo-keyed,
never task-gating): a cheap single-tool authoring probe whose only job is to
*corroborate* a registry parity verdict and feed the §4c drift detector a current
reading (the role the blog-draft assigns OpenRouter's Tool Call Error Rate). It
never blocks or delays the real first task; if it is skipped or stale, the static
gate + reactive self-heal still fully cover the route. Implementation reuses the
`mock`/`fake` provider seam and the `tool_conformance` round-trip harness — it is
the live-route sibling of the offline conformance test, not a new code path. It is
explicitly lower priority than (b2)/(b3) in the phased plan and may be dropped
entirely if the static+reactive pair proves sufficient on the meter.

**(b2) In-session tool-stream-integrity guard — the core deliverable.**
After a turn's response is normalized but *before* the loop accepts it, run a
deterministic, name-free integrity classifier over the `LlmResult` + request
signals and emit a typed `ToolStreamIntegrity` verdict:

- `Intact` — has a parseable call (or a legitimate text answer to a tool-less
  turn). No-op.

- `VanishedOnNative` — tools were offered, native channel, clean finish, zero
  parsed calls (this is S2; it reuses `is_billed_noncommittal_completion`'s exact
  signal set rather than re-deriving it).

- `MarkupInContent` — content carries `<tool_call>`/DSML/Harmony markers that did
  not parse into `tool_calls` on this channel (S1/S3 that slipped the pre-steer).

- `CorruptArguments` — a call parsed but its arguments failed schema validation
  in a way consistent with escape corruption (S4/S5).

The verdict drives recovery (b3). The guard is **paradigm-agnostic**: in code
mode the same guard watches the snippet's child-call stream; a snippet that
emitted zero child calls when the task required one is the code-mode analogue of
`VanishedOnNative`.

**(b3) Closed-loop auto-recovery — a TIERED remedy ladder, cheapest first.**
The single most important property of the recovery layer is **remedy ordering**:
a channel switch invalidates the entire prefix KV cache, and two failure
signatures mapping to two channels can oscillate (the OpenCode #18108 doom-loop).
So channel-switch is the *last* resort, never the first reflex. The ladder,
cheapest first:

1. **Continue-on-truncation (no cache cost).** `finish_reason == length`/
   `max_tokens` with a *valid tool name but incomplete arguments* is a budget
   problem, not a broken channel: CONTINUE the loop / raise the token budget and
   let the model finish the call. This MUST sit above every other tier. It is
   already implemented one layer up (`agent_session_host` auto-continue on length
   truncation with a partial call, and `api/result.rs` distinguishes truncated
   args from genuinely-malformed) — the RFC ratifies it as tier 1 and the
   self-heal layer must never let a truncation fall through to a degrade. The
   structural detectors (`is_billed_noncommittal_completion`,
   `is_zero_token_empty_completion`, `is_native_tool_channel_failure`,
   `is_billed_noncommittal_throw`) all explicitly exclude `stop_reason == length`,
   so a truncation can never reach the degrade trigger — a regression test
   (`truncation_does_not_trigger_channel_degrade`) pins this invariant.

2. **Bounded same-channel retry.** Empty / vanishing completion with no
   tool-channel fingerprint (the bare provider stall, `delivered no content`) is a
   transient hiccup: retry on the *same* channel within the bounded
   empty-completion budget. Already implemented.

3. **Channel-switch — LAST RESORT, ONE-WAY per session.** Only when the failure
   signature says the *native channel itself is broken for this route* (a 5xx/EOF
   tool-parser choke, the billed-noncommittal vanish, or a native function-call
   protocol refusal) does the loop degrade native → text. Because a switch dumps
   the prefix cache, it fires **at most once per call** (the existing
   `degraded_to_text` latch) and never switches back (one-way), so two signatures
   can never oscillate it.

**Generalizing the #3500 degrade (tier 3 trigger).** Today
`degrade_options_to_text_channel` fires only on `is_native_tool_channel_failure`
(thrown 5xx/EOF). Generalize the *trigger* to any signature/verdict that names a
broken native channel, while keeping the *action* (the in-place, one-way degrade)
and its once-only, native-channel-only guard. Concretely the degrade also fires
when:

- the `Err` arm sees a billed-noncommittal throw (S2) **or** a native
  function-call protocol refusal (S6, the SambaNova 400) on a native channel —
  the highest-leverage change: `is_native_tool_channel_failure(err) ||
  is_billed_noncommittal_throw(err)` (SHIPPED in Phase 1; see §5);

- the `Ok` arm's integrity guard returns `VanishedOnNative` or `MarkupInContent`
  on a native channel — promote the silent `Ok` into the same one-way degrade path
  the thrown shapes use.

**Abort+restart vs in-place degrade — the prefix-cache cost analysis (honest):**
The #3500 degrade is *in-place*: it mutates the working `LlmCallOptions` and
re-sends *the same turn*. That is correct and cheap because the degrade happens
*within* `observed_llm_call`, before the turn is committed to the transcript —
there is no transcript history to re-render, and the prompt prefix (system +
tools + prior turns) is identical between the native attempt and the text attempt
*except* for the tool-rendering tail. So in-place degrade pays: (a) one wasted
native attempt's output tokens, (b) a partial prefix-cache miss on the
tool-rendering tail only (the bulk system+history prefix still hits cache on the
retry). This is strictly cheaper than abort+restart, which would re-run the whole
session from turn 0. **Therefore: prefer in-place degrade for the current-turn
failure (S2/S3/S5 caught before commit). Reserve abort+restart only for the case
where a bad channel was *already committed* to the transcript across multiple
turns before detection** — there, the transcript holds native-only tool messages
that can't be replayed under a text contract (the `docs/tool-format-contracts.md`
cross-format rule), so a safe recovery *must* reset/compact and restart on the
corrected channel. The decision rule: detected pre-commit → in-place degrade;
detected post-commit-with-history → compact-and-restart. The guard runs
pre-commit, so the common case is always the cheap one.

The recovery loop must remain **bounded and observable**: once per call (already
true), emit a `tool_format_degrade` observability entry + span annotation
(already true), and — new — feed the outcome to the §4c drift detector.

**(b4) Feedback-enrichment tier — BELOW escalation, the SOTA gap we are missing.**
The recovery ladder above fixes *wire/channel* failures. The far more common
agent failure is a *semantic* one: the model wrote a call or an edit that
dispatched fine but did the wrong thing, and the verifier said "failed." The
SOTA-correct response there is **not** "retry more" and **not** "escalate to a
frontier model" first — it is **enrich the feedback**. Olausson et al. (ICLR
2024) show that better feedback beats more retries: a cheap model given a
*structured* error class + the failing assertion + the exact line it wrote + the
diff will often self-correct on the next turn, removing the need to escalate at
all. Escalation is expensive; enriched feedback is nearly free and we already own
the materials:

- the **`DiagnosticEnvelope` / `Diagnostic` structs (#2677)** give a typed error
  class instead of a raw build-log dump;

- the **`render_edited_window` / edited-window renderer** gives the exact lines the
  model wrote with the failing region highlighted;

- the dependency/manifest diagnostics (#2678) give the cross-ecosystem "you reached
  for a dep that doesn't exist" class.

So the full remedy stack, cheapest-to-most-expensive, is:
**(1) continue-on-truncation → (2) bounded same-channel retry → (3) one-way
channel-switch (wire failures) → (4) feedback-enrichment (semantic failures) →
(5) escalation to a stronger model (last resort).** Feedback-enrichment sits
*below* escalation precisely because it frequently *prevents* escalation. This RFC
ratifies (4) as a first-class tier and wires its outputs (structured diagnostic,
edited window, diff) into the verifier feedback the cheap model sees.

**Source-level prevention (note, not a tier).** For malformed *native* calls
specifically, constrained/grammar-constrained decoding —
`tool_choice = "required"` and provider grammar constraints — prevents the bad
call at generation time rather than repairing it after. Where a route supports it
(`allowed_tool_choice_modes` already records this in the registry), prefer it: a
call that can't be malformed never needs tiers 1–4. This is the upstream
complement to the reactive ladder.

### (c) A living capability registry — recommendation: BUILD the drift detector (propose; gate still disposes)

The registry is only as good as its facts, and providers change behavior
silently. Make the registry self-auditing without making it self-mutating.

- **Signal.** Every gate decision, every integrity verdict (b2), every degrade
  (b3), and the optional advisory probe (b1) already produces — or will produce — a
  per-route tool-call health datum: parsed-call count, parse-rejection count, the
  vanishing-`[]` mode, byte-fidelity pass/fail, degrade-fired. Roll these into a
  per-route windowed parse-success / dispatch-success rate (a ring buffer keyed by
  `(provider, model_match, channel)`), persisted to the run-events / events.sqlite
  sink so it survives across runs.

- **Detector.** When a route the registry currently classifies as healthy
  (`interchangeable` / `native` preferred) drops below a floor over a meaningful
  window — corroborated where available by OpenRouter's external Tool Call Error
  Rate — the detector does **not** flip the TOML. It emits a *proposed* diff
  against the capability source (`tool_mode_parity` / `preferred_tool_format`)
  with the supporting sample, as a reviewable artifact (a `harn providers
  drift-proposals` command + a machine-readable proposal record).

- **Disposition.** A human (or, behind an explicit opt-in flag, an auto-disposer
  bounded to *demotions only*) accepts the proposal, which regenerates
  `capabilities.toml` through the normal `capability_sources` path. The enforced
  gate (#3462) is unchanged and remains the only thing that *binds* — the detector
  only changes what facts the gate reads, and only through review.

- **Asymmetry (mirrors the gate's own rule).** Auto-*propose* a demotion (native
  → text) on positive evidence of breakage; never auto-propose a *promotion*
  (text → native) — recovering a route to native requires a deliberate forced
  probe sweep, because a few clean turns are not evidence a flaky native channel
  is fixed. This keeps the failure mode "a route stays conservatively on text
  longer than strictly necessary," never "a route silently regresses to a broken
  native pin."

This closes the blog-draft's #1 open question ("how do we keep parity facts
fresh") with a concrete, conservative mechanism.

### (d) Model-switch mid-transcript channel re-resolution — recommendation: re-resolve + safely re-render or restart

Escalation/failover changes the model mid-transcript; the new model may need a
different channel. The format-contract invariant already forbids replaying a
text-protocol transcript under a native contract (and vice versa). The north-star
behavior:

1. On a model switch, re-run format resolution (the static gate; optionally the
   advisory probe) for the *new* `(provider, model)`.

2. If the resolved channel equals the session's claimed `tool_format` → proceed
   (the common case: escalating Sonnet→Opus stays native).

3. If it crosses (text↔native) → do **not** silently replay. Per the contract,
   either (a) compact/summarize the prior transcript into a fresh session that
   claims the new channel (preserves semantic history, drops channel-specific tool
   messages), or (b) start a child session on the new channel with lineage back to
   the parent. This must be a first-class, automatic path inside the
   escalation/failover machinery, not a caller responsibility — the whole point of
   the north star is that no consumer thinks about channels.

4. The choice between compact-and-reclaim vs child-session is driven by whether
   the prior history is load-bearing for the escalated task (compact when the
   escalation needs the context; child when it's a fresh sub-task). Default:
   compact-and-reclaim, because escalation almost always needs the context that
   triggered it.

## 5. Phased implementation plan (what ships first, what's the breaking cutover, how each phase is gated)

Each phase is independently shippable and independently valuable. **Every phase
is non-breaking**: code-mode (Phase 5) ships as a tier-gated opt-in layered on the
native default, never a cutover of the default loop.

**Phase 0 — Evidence + RFC (this document).** Ships the taxonomy and the plan.
Gate: merged RFC + markdown lint. (No code risk.)

**Phase 1 — Generalize the reactive self-heal trigger (NON-breaking, SHIPPED in
this RFC's companion PR).**

- 1a. **DONE.** Added `is_billed_noncommittal_throw` and widened the
  `native_tool_channel_degrade` trigger in `observed_llm_call` to fire on S2
  (billed-noncommittal vanish) and S6 (SambaNova native function-call refusal, a
  4xx the #3500 5xx/EOF predicate deliberately missed), in addition to the
  existing 5xx/EOF parser choke. Unit tests for the predicate edges + two
  mechanism-fitness integration tests (billed-noncommittal and SambaNova-refusal
  each degrade to text and recover) + the remedy-ladder invariant test
  (`truncation_does_not_trigger_channel_degrade`, pinning continue-on-truncation
  above channel-switch). All green; clippy `-D warnings` clean on `harn-vm`.

- 1b. (next) Add the `ToolStreamIntegrity` classifier (b2) and wire the `Ok`-arm
  `VanishedOnNative`/`MarkupInContent` verdicts into the same one-way degrade path.

- **Meter gate:** Phase 1 is a convergence claim (it changes cheap-model agent
  behavior). Follow `docs/eval/meter-stick.md`: iterate on `meter-tune`, gate on
  frozen `meter-holdout`, N≥5 for the primary cheap model, paired 95% CI lower
  bound `> 0` to claim improvement. The mechanism-fitness mini-evals (scripted
  fixtures) ship FIRST, before any cloud spend, per the AGENTS.md discipline —
  1a's tests are exactly that.

**Phase 2 — Feedback-enrichment tier (NON-breaking).**

- Wire `DiagnosticEnvelope` (#2677) + the edited-window renderer + dependency
  diagnostics (#2678) into the verifier feedback the cheap model sees on a
  semantic (non-wire) failure, as tier 4 of the remedy stack (below escalation).

- Also enable constrained decoding (`tool_choice = "required"` / grammar) on routes
  whose `allowed_tool_choice_modes` supports it — source-level prevention of
  malformed native calls.

- Mechanism-fitness: scripted failing edit → assert the next-turn feedback carries
  the structured diagnostic class + the failing assertion + the edited window +
  the diff (not a raw log); assert enriched feedback is attempted BEFORE an
  escalation fires.

- **Meter gate:** convergence claim — does enriched feedback raise cheap-model
  pass@1 and REDUCE escalation rate / cost-per-solved? Paired CI lower bound `> 0`,
  N≥5.

**Phase 3 — Optional advisory channel probe (NON-breaking, LOW priority, may be cut).**

- Channel micro-probe (b1), `(provider, model)`-keyed (NOT repo-keyed), advisory
  only, never task-gating. Reuses the `mock`/`fake` seam + `tool_conformance`
  harness. Feeds the §4c drift detector a current reading.

- Mechanism-fitness: scripted route where the native probe fails / text probe
  passes → assert the probe emits a corroborating demotion signal to the drift
  detector; negative: assert the probe NEVER blocks or delays the real first task,
  and a healthy route is skipped.

- **Meter gate:** measure that the probe does not regress cost/latency. If the
  static registry + reactive self-heal already cover the routes (likely), this
  phase may be dropped entirely — it is the lowest-priority item in the RFC.

**Phase 4 — Living registry drift detector (NON-breaking; proposes only).**

- Per-route health ring buffer + `harn provider catalog drift-proposals` + proposal
  records. Gate stays the only binding layer.

- Mechanism-fitness: feed a scripted stream of vanishing-call data for a
  currently-healthy route → assert a *proposal* is emitted (not a TOML flip);
  feed healthy data → assert no proposal. Assert a promotion is NEVER auto-proposed.

- **Meter gate:** not a convergence claim by itself (it changes no runtime
  behavior until a human disposes); gate is the unit/mechanism-fitness suite +
  a manual review that a real stale pin (e.g. a route that has since regressed)
  produces a correct proposal.

**Phase 5 — Code-mode as a TIER-GATED opt-in layer (NON-breaking; default stays native).**

- Add the `code_mode_capable` registry fact (frontier-tier on; cheap lane off) +
  the tier gate ABOVE code-mode's existing read-only/AutoApprove gate. For capable
  models on a large/deferred surface, code-mode becomes an *available* layer the
  loop may enter; the DEFAULT remains native structured calls for every route,
  including the entire cheap lane. Nothing is cut over.

- Mechanism-fitness: scripted capable model authors a valid snippet → child calls
  dispatch + audit intact; scripted weak model (tier-gated OFF) never enters
  code-mode; a capable model that fails snippet authoring N times auto-demotes back
  to the native default for the session.

- **Meter gate (the big one), PER MODEL TIER:** turning code-mode ON for a tier's
  *default* path would be a convergence claim and must clear, per tier: held-out
  macro pass@1 paired 95% CI lower bound `> 0` vs the native-call baseline, N≥5,
  plus worst-language pass@1, mean cost, mean wall time, and routing metrics
  (escalation rate, over/under-escalation, cost-per-solved). Until a tier clears
  this, code-mode stays opt-in for that tier and native stays its default. The
  cheap lane is expected to FAIL this gate (CodeAct inversion) and correctly stay
  native.

**Phase 6 — Model-switch channel re-resolution (NON-breaking).**

- Wire (d) into the escalation/failover path: re-resolve on switch, compact-and-
  reclaim on cross-format.

- Mechanism-fitness: scripted escalation that crosses text→native → assert a
  compact-and-reclaim (not a silent replay) and a clean run on the new channel;
  same-channel switch → assert a plain proceed.

- **Meter gate:** measure escalation convergence@frontier — does a cross-format
  escalation now *complete* where it previously dead-ended on a contract error.

## 6. Explicit recommendation

The single sentence: **keep native structured tool-calls the default, make the
static registry + reactive self-heal own the dialect, and offer code-mode only as
a tier-gated opt-in for scale.** Ship in this order, treating Phase 1 as urgent:

1. **Phase 1 now (SHIPPED 1a).** Generalizing the #3500 degrade to fire on the
   billed-noncommittal vanish (S2) and the SambaNova native function-call refusal
   (S6) closes the two most common hard tool-call failures in the sweep (13 + 6
   occurrences) with a change measured in tens of lines, fully composing with
   `is_billed_noncommittal_completion` which *already exists* one layer down — and
   it is correctly subordinate to continue-on-truncation in the remedy ladder. This
   is the foundational non-breaking layer that stops the whack-a-mole.

2. **Phase 2 (feedback-enrichment)** is the cheapest convergence lever and often
   *removes the need to escalate* — ship it before leaning on escalation.

3. **Phases 3–4** make the system current (advisory probe, low priority) and
   self-auditing (living registry; propose-demotions-only).

4. **Phase 5 (code-mode)** is the scale lever, NOT the default. It ships as a
   tier-gated opt-in layered on the native default. Flipping it to a tier's default
   is a per-tier convergence claim with a paired CI lower bound above zero; the
   cheap lane is expected to stay native (CodeAct inversion), and that is the
   correct outcome, not a failure.

5. **Phase 6** finishes the hands-off escalation story (cross-format model switch).

The bold claim, stated carefully: with the reactive self-heal ladder (Phase 1)
plus the living registry (Phase 4), a consumer can point Harn at *any* route —
including a brand-new one whose registry fact is wrong — and a vanishing tool
stream either never happens (the static gate pre-steers) or self-heals within the
same call (one-way degrade) and surfaces a registry-update proposal for next time.
That is
Harn owning the dialect with a native-default loop. Code-mode (Phase 5) is the
optional, tier-gated layer that makes the owned surface context-efficient at scale
for the capable models that can wield it — not a bet we place on the cheap lane.
Together they make Harn the best hands-off orchestration toolchain on the planet.

## Appendix A — Evidence excerpts (byte-level, from the 2026-06-18 → 06-24 sweep)

Inventory: two live eval roots —
an IDE-host eval corpus (895 run dirs) and
`/private/tmp/bc-hardcell-harness/.burin-evals` (8). Layout per eval: base dir +
`-llm` (`llm_transcript.jsonl`: `provider_call_request/response/error` records) +
`-events` (`event_log-*.jsonl`, `topics/agent.transcript.llm.jsonl`); trials roll
up to `trials-<model>-<ts>.json` with a `transcript_mining_report` pointer.

**S6 — SambaNova tool-protocol failure (6 calls, aborted run), tf=native, 9 tools.**
`…/eval-zig-feat-20260624-141259-5aebef1d-1ff-llm/llm_transcript.jsonl`:

```text
sambanova HTTP 400 Bad Request [invalid_request]: Model started a function call but did not complete it.
```

**S2 — DeepInfra billed-noncommittal (13), tf=native, 9 tools.**
`…/eval-zig-feat-20260624-140915-9f75640d-07d-llm/llm_transcript.jsonl`:

```text
provider deepinfra … returned billed output (completion_tokens=86) with no dispatchable tool call or answer (upstream contract violation): the model finished cleanly but committed neither a tool call nor visible text. This usually means the route serialized the action only in a private reasoning channel…
```

The error message itself prescribes the Phase 1 fix: "prefer a Harn text/json
tool format."

**S2 — Fireworks reasoning-only, response-level (5), `stop=length`.**
`…/eval-scala-test-20260622-083305-3172c4fc-4fa-t4-llm/llm_transcript.jsonl` —
empty text + empty tool_calls, ~11k-char `thinking`:

```text
{"stop":"length","out":640,"ntc":0,"text":"","think_head":"We need to decide if the assistant's latest visible response is a final answer…"}
```

**S3 — GLM-5.1 `<tool_call>` markup runaway in a JSON judge call (1), `stop=length`.**
`…/eval-zig-feat-20260620-141802-3ba08b65-fb6-t1-llm/llm_transcript.jsonl` —
literal `<tool_call>` repeated to token cap, JSON quotes double-escaped; this is a
structured-output *judge* call, not the agent tool loop:

```text
{"verdict":"done\",\"reasoning\":\"…emitted ##DONE##.\",\"next_step\":\"\"}<tool_call><tool_call><tool_call>…
```

**S7 — Groq free-tier TPM 413 (9; 3 exhausted).**
`…/eval-zig-feat-20260624-140746-1dc97011-13f-llm/llm_transcript.jsonl`:

```text
groq HTTP 413 Payload Too Large [rate_limited]: Request too large … tokens per minute (TPM): Limit 8000, Requested 10737 … (retry-after: 71)
```

**S9 — Anthropic transcript-replay reject (6).** A stored reasoning field re-sent
to the API; a transcript-replay bug, separate from open-weight dialect.
`…/eval-zig-feat-20260622-135413-5fb6d7aa-9cc-llm/llm_transcript.jsonl`:

```text
anthropic HTTP 400 [invalid_request]: messages.1.reasoning: Extra inputs are not permitted (type: invalid_request_error)
```

**N1 — Cerebras gpt-oss-120b native, CLEAN (negative control).** 58/65 calls
returned exactly one well-formed native `tool_call`, 0 `provider_call_error` rows;
its only failure was `edit_applied_but_wrong` (a *capability* failure, not a
tool-call one). Confirms native gpt-oss is route-dependent, validating §4c's
never-auto-promote / only-auto-propose-demotions asymmetry.

**Negative results that pruned the design:**

- "True vanishing `tool_calls`" (`stop=tool_calls` + empty array): **0** across all
  50 transcripts — the integrity guard keys on the billed-noncommittal signal set
  instead.

- gpt-oss Harmony `\\` double-escape in agent bodies: **not** reproduced (canonical
  Fireworks runs are tf=text; minimax-m2.7 on tf=json carried 205 `\\` Zig escapes
  with zero halving — the catalog `json` pin holds).
