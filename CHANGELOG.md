# Changelog

All notable changes to Harn are documented in this file.

Prior-series highlights (pre-0.6) are condensed at the bottom. Harn had no
external users before 0.6.0, so we intentionally do not preserve the full
per-patch history of the 0.5.x and 0.4.x lines here — consult `git log` for
granular archaeology.

## Unreleased

### Added

- **Skills & Tool Vault phase 3: `agent_loop` skill lifecycle (harn#74).**
  `agent_loop` now accepts a `skills:` option (a `skill_registry`
  produced by the `skill { }` top-level form or `skill_define(...)`)
  and runs a match-activate-reassess phase around every turn. The
  default metadata matcher scores skills by BM25-ish keyword overlap
  over `description` + `when_to_use`, name-in-prompt mentions, and
  `paths:` glob matching against the host-supplied `working_files:`
  list; opt into host-delegated ranking (embedding / LLM scorers /
  whatever) via `skill_match: { strategy: "host" }` or `"embedding"`
  — both route through a new `skill/match` JSON-RPC bridge method.
  - Activation binds the skill's `prompt` body into the effective
    system prompt, narrows the tool surface via its `allowed_tools`
    whitelist (union when multiple skills are active), and calls
    its `on_activate` hook. Deactivation (in `sticky: false` mode)
    unwinds everything and calls `on_deactivate`.
  - `disable-model-invocation: true` and `user-invocable: false`
    SKILL.md frontmatter are honoured: the matcher skips disabled
    skills entirely; `user-invocable` rides through for host UIs.
  - Transcript events `skill_matched`, `skill_activated`,
    `skill_deactivated`, `skill_scope_tools` emit with stable
    schemas. The first three also emit as `AgentEvent` variants so
    ACP hosts see live session updates (`harn-cli`'s ACP server
    translates them into `session/update` notifications).
  - Session-resume: when `session_id:` is set, the active skill set
    at the end of one run is persisted in the session store and
    rehydrated on the next `agent_loop` invocation, skipping
    iteration-0 match so sticky re-entry stays hot.
  - Conformance coverage under `conformance/tests/skill_lifecycle_*`.
- **Skills phase 2: filesystem `SKILL.md` loader + layered discovery (harn#73).**
  `harn run` / `harn test` / `harn check` now pre-populate the `skills`
  VM global with every `SKILL.md` they find across eight priority
  layers: `--skill-dir` (CLI), `$HARN_SKILLS_PATH` (env),
  `.harn/skills/` (project), `harn.toml` `[skills] paths` &
  `[[skill.source]]` (manifest), `~/.harn/skills` (user),
  `.harn/packages/**/skills/` (package), `/etc/harn/skills` &
  `$XDG_CONFIG_HOME/harn/skills` (system), bridge-registered (host).
  Frontmatter follows Anthropic / Claude-Code's Agent Skills spec
  (`name`, `description`, `when-to-use`, `disable-model-invocation`,
  `allowed-tools`, `user-invocable`, `paths`, `context`, `agent`,
  `hooks`, `model`, `effort`, `shell`, `argument-hint`); unknown
  fields surface as `harn doctor` warnings, not hard errors, so the
  spec can evolve without breaking older VMs. New `skill_render(skill,
  args)` builtin applies `$ARGUMENTS` / `$N` / `${HARN_SKILL_DIR}` /
  `${HARN_SESSION_ID}` substitutions to the SKILL.md body. Bridge
  protocol gains `skills/list` + `skills/fetch` requests and a
  `skills/update` notification for host-driven hot-reload. See
  `docs/src/skills.md` for the full reference and
  `docs/src/bridge-protocol.md` for the wire format.
- **Tool Vault phase 3: OpenAI Responses-API native `tool_search` (harn#71).**
  `tool_search` now flows through OpenAI's native progressive-disclosure
  mechanism on GPT 5.4+ with zero script changes: the capability gate
  detects the model generation (via `gpt_generation()` — parses
  `gpt-5.4-preview`, `gpt-5.4-turbo`, `gpt-5-4-20260115`, and
  OpenRouter-style `openai/gpt-5.4` prefixes), prepends the meta-tool
  `{"type": "tool_search", "mode": "hosted"}` to the tools array, and
  emits `defer_loading: true` on each deferred user tool's wrapper.
  Server-executed `tool_search_call` / `tool_search_output` entries in
  the response get parsed into the same `tool_search_query` /
  `tool_search_result` transcript events as the Anthropic path —
  replays are indistinguishable across providers. OpenRouter, Together,
  Groq, DeepSeek, Fireworks, HuggingFace, and `local` all inherit the
  same capability check; when their routed model ID matches `gpt-5.4+`
  they forward the payload unchanged.
- **`namespace: "<label>"` on `tool_define(...)`** groups deferred tools
  for OpenAI's `tool_search` meta-tool. Distinct namespaces are
  collected into the meta-tool's `namespaces` field (sorted, deduped).
  Anthropic ignores the label — harmless passthrough for replay
  fidelity. Type-validated: non-string values error at `tool_define`
  time so typos surface immediately.
- **Escape hatch `<provider>: {force_native_tool_search: true}`** on
  call options forces the hosted OpenAI path regardless of model
  detection. Useful for self-hosted routers and enterprise gateways
  whose model IDs Harn cannot parse but that forward `tool_search` +
  `defer_loading` unchanged.
- **Mock provider spoofs native capability by model generation.** When
  a conformance test writes `provider: "mock", model: "gpt-5.4"` or
  `"claude-sonnet-4-6"`, the capability gate reports native support so
  the test can exercise the real native payload shape via
  `llm_mock_calls()[0].tools`. Non-matching models still report no
  native support (used by `tool_search_unsupported_provider.harn`).
- **Response-parser coverage for OpenAI `tool_search_call` /
  `tool_search_output`.** Both non-streaming and SSE streaming paths
  now strip these blocks from the dispatchable `tool_calls` vector
  (they're server-executed) and record them as transcript events with
  the same shape Anthropic's `server_tool_use` /
  `tool_search_tool_result` emits. The empty-response sanity check
  exempts calls whose output consists entirely of these blocks.
- **New `crates/harn-vm/src/llm/providers/openai_compat.rs` helpers.**
  `gpt_generation(model)` parses major/minor from GPT model IDs;
  `gpt_model_supports_tool_search(model)` gates on `(major, minor) >=
  (5, 4)`. Unit-tested on dotted (`gpt-5.4`), dashed (`gpt-5-4`),
  namespaced (`openai/gpt-5.4-turbo`), and dated
  (`gpt-5-20260115` → `(5, 0)`, unsupported) forms.
- **Conformance tests.** `tool_search_native_openai.{harn,expected}`
  verifies the native injection + deferred-flag passthrough +
  unsupported-model diagnostic. `tool_search_namespace.{harn,expected}`
  verifies namespace passthrough through the registry, into the
  OpenAI wrapper, and into the meta-tool's `namespaces` field.
  `tool_search_provider_overrides.{harn,expected}` verifies the
  escape hatch.
- **Tool Vault phase 2: universal client-executed `tool_search` fallback (harn#70).**
  `tool_search` now works on every provider, not just the
  Anthropic-native path landed in phase 1. When the active provider
  lacks native `defer_loading` (Gemini, Ollama, OpenAI pre-5.4,
  Together, Fireworks, Groq, Deepseek, HuggingFace, local, mock),
  Harn auto-switches to an in-VM fallback: a synthetic
  `__harn_tool_search` tool is injected, the deferred tools are
  stripped from the initial turn's schema list, and when the model
  calls the synthetic tool the configured strategy runs against the
  deferred-tool corpus and the matching tools get promoted onto the
  *next* turn's schema list. The option surface is unchanged —
  `tool_search: "bm25"` / `"regex"` / `true` / `{variant, mode, ...}`
  all Just Work on any provider. `mode: "auto"` falls back silently;
  `mode: "client"` forces the fallback even on native-capable
  providers.
- **Four client-mode strategies.**
  - `"bm25"` (default) — tokenized BM25 over tool
    `name + description + parameter text`, matching Anthropic's native
    ergonomic for cross-provider consistency.
  - `"regex"` — case-insensitive Rust-regex over the same corpus
    (no backreferences / lookaround; see the regex crate docs).
  - `"semantic"` — delegates to the host via a new
    `tool_search/query` bridge RPC so integrators can wire embeddings
    without Harn depending on ML crates.
  - `"host"` — same RPC shape as semantic; the host decides how to
    rank. The VM round-trips the query and promotes whatever names
    come back.
- **New client-mode knobs on `tool_search`.** `budget_tokens: N`
  (soft cap with oldest-eviction for promoted schemas),
  `name: "find_tool"` (rename the synthetic search tool so skills can
  pick a verb the model prefers), `include_stub_listing: true`
  (append a short list of deferred-tool names to the contract prompt
  so the model can eyeball what's available without a search call),
  and `strategy: "..."` (explicit strategy override independent of
  `variant`, so you can pick a BM25-framed prompt with a semantic
  backend, for example).
- **`tool_search/query` bridge RPC.** Standard JSON-RPC request
  issued by the VM for `strategy: "semantic"` / `"host"`. Payload:
  `{strategy, query, candidates}`; response: `{tool_names, diagnostic?}`
  (or the ACP wrapper `{result: {...}}`). Documented in
  `docs/src/bridge-protocol.md`.
- **Cross-provider transcript parity.** Client-mode
  `tool_search_query` / `tool_search_result` events use the same
  shape as the Anthropic-native path — id, name, query / tool_use_id,
  tool_references — so replayers and analytics stay agnostic.
  Metadata adds `mode: "client"` tagging for distinguishing paths
  when that matters.
- **New `crates/harn-vm/src/llm/tool_search/` module.** In-tree BM25
  and regex indices with per-strategy tests. BM25 uses the conventional
  `k1 = 1.5`, `b = 0.75`; tokenization splits on non-alphanumeric
  boundaries so `open file` matches `open_file`.

### Changed

- **`tool_search_unsupported_provider.harn` pins `model: "gpt-4o"`**
  (phase 3 / harn#71) so it continues to error on `mode: "native"`
  after mock capability spoofing. The diagnostic still suggests
  `mode: "client"` as the escape hatch; the error text is unchanged.
- **Client-mode conformance tests now use `mode: "client"`
  explicitly** (phase 3 / harn#71). With mock spoofing a Claude 4.0+
  or GPT 5.4+ model, `mode: "auto"` would otherwise route through a
  native path. The tests name themselves `tool_search_client_*`;
  they now opt into the path they claim to cover.
- Non-Anthropic providers no longer error when the user opts into
  `tool_search`. The phase-1 "no silent degradation" diagnostic that
  previously pointed at harn#70 is replaced by the actual fallback
  behavior. The `mode: "native"` explicit-intent path still errors on
  providers without native support (its error message now suggests
  `mode: "client"` as the escape hatch).
- `tool_search_unsupported_provider.harn` conformance test adjusted
  to match the new behavior (only `mode: "native"` on mock still
  errors).

## v0.7.17

### Added

- **Skills are a first-class top-level form.** Adds `skill NAME { ... }`
  alongside `pipeline` / `fn` / `tool`. Each body entry is a
  `<field_name> <expression>` pair; lifecycle hooks
  (`on_activate fn() { ... }`, `on_deactivate`) are ordinary fn-literal
  expressions. The decl lowers to
  `skill_define(skill_registry(), NAME, { field: value, ... })` and
  binds the resulting registry dict to `NAME`. New stdlib module
  `crates/harn-vm/src/stdlib/skills.rs` exposes `skill_registry`,
  `skill_define`, `skill_list`, `skill_find`, `skill_select`,
  `skill_remove`, `skill_count`, `skill_describe`. `skill_define`
  validates known-key value shapes (`description`/`when_to_use`/
  `prompt`/`invocation`/`model`/`effort` as strings;
  `paths`/`allowed_tools`/`mcp` as lists) so typos raise at
  registration rather than at use. Attribute sugar `@acp_skill(name:
  "...", when_to_use: "...", invocation: "explicit", ...)` applied to
  a `fn` registers the fn as the skill's `on_activate` hook and lifts
  the remaining named args into the skill metadata. Covered by
  `conformance/tests/skill_decl.{harn,expected}`,
  `conformance/tests/attributes_acp_skill.{harn,expected}`, and
  `conformance/errors/skill_define_invalid.{harn,error}`. Coordinated
  updates to lexer (new `skill` keyword), parser (new `SkillDecl` AST
  with `fields: Vec<(String, SNode)>`), tree-sitter grammar + tests,
  VS Code syntax highlighter and snippets, spec, and quickref. Closes
  [#72](https://github.com/burin-labs/harn/issues/72).

- **Debugger M1–M4: DAP surface reaches protocol parity.** Adds the full
  Debug Adapter Protocol feature set needed for IDEs to drive Harn runs
  as first-class debug sessions. Capabilities advertised:
  `supportsLogPoints`, `supportsHitConditionalBreakpoints`,
  `supportsConditionalBreakpoints`, `supportsSetVariable`,
  `supportsSetExpression`, `supportsFunctionBreakpoints`,
  `supportsRestartFrame`, `supportsCompletionsRequest`,
  `supportsStepInTargetsRequest`, `supportsCancelRequest`,
  `supportsInvalidatedEvent`, plus Burin-namespaced
  `supportsBurinPromptProvenance`. `exceptionBreakpointFilters` expands
  to `{all, tool_error, llm_refusal, budget_exceeded, parse_failure}`
  with optional per-filter conditions (both legacy `filters` and
  DAP-1.58 `filterOptions` supported). Specific landings:
  - **#85 unified frame evaluator.** `Vm::evaluate_in_frame` /
    `set_variable_in_frame` / `restart_frame` with 10k-step budget,
    VM state snapshot/restore — powers hover, watches, conditional
    BPs, `setVariable` / `setExpression`, logpoint message rendering.
  - **#86 multi-thread readiness.** Per-`Debugger` thread registry
    seeded with `{1 → "main"}`; `threadStarted` / `threadExited`
    events; stepping / pause / exception events carry the live
    `threadId` instead of hardcoded 1.
  - **#87 logpoints.** `SourceBreakpoint.logMessage` renders via
    `{var}` interpolation without stopping.
  - **#88 hit-count breakpoints.** `hitCondition` parsed in
    `N / >=N / >N / %N` forms; counts reset on run enter and BP
    edits.
  - **#89 conditional breakpoints.** `SourceBreakpoint.condition`
    evaluated via the unified frame evaluator.
  - **#90 function breakpoints.** `setFunctionBreakpoints` stops on
    entry to any closure whose name matches; re-applied on launch so
    they survive relaunch.
  - **#91 `setVariable` / `setExpression`.** Mutate scope while
    paused, bypassing let-immutability via `VmEnv::assign_debug`.
  - **#92 `restartFrame`.** Rewinds `ip` and restores the
    `initial_env` snapshot captured at every call site.
  - **#93/#94 prompt provenance MVP.** `PromptSourceSpan` +
    thread-local `PROMPT_REGISTRY`; `render_with_provenance` builtin
    returns `{text, template_uri, prompt_id, spans}`; custom
    `burin/promptProvenance` and `burin/promptConsumers` DAP
    requests expose the registry over the wire.
  - **#102 triggered breakpoints.** `Breakpoint.triggeredBy: [id]`
    arms a BP only after a listed dependency BP has fired; armed
    state clears per run. Pattern: "break on the second turn's
    first `tool_error`".
  - **#108 `cancel` request.** Dispatches DAP cancel to both the
    `DapHostBridge`'s pending reverse-request waiter and a new
    `Vm::install_cancel_token` / `signal_cancel` cooperative token.
    The step loop polls on every instruction and unwinds with a
    `kind:cancelled:` `Thrown` that flows through the exception
    filter pipeline.
  - **#109 completions.** `Vm::identifiers_in_scope(frame_id)`
    unions frame locals with every registered builtin / async
    builtin; filtered by prefix, capped at 200.
  - **#110 `invalidated` events.** Helper builds the DAP
    `invalidated` event carrying areas + `current_thread_id`.
  - **#111 per-kind exception filters.** `extract_exception_kind`
    plus `exception_filter_matches` route `kind:<name>:` throws
    through the selected filter; stopped event carries
    `hitBreakpointIds: [kind]`.
  - **#112 `stepInTargets`.** Call-family ops on the current line
    enumerate per-target step-in IDs (`frame_id × 1e6 + index`).
- **Cross-template provenance chains (#96).** Every span emitted by
  `render_template_with_provenance` gets a `parent_span` +
  `template_uri`, so `include` traversal builds a walkable A→B→C
  chain. `burin/promptProvenance` surfaces the recursive chain plus
  `rootTemplateUri`, letting the IDE render cross-template
  breadcrumbs that click through to the inner source.
- **JSONL `AgentEvent` persistence (#103).** New `JsonlEventSink`
  writes an append-only `event_log-*.jsonl` stream with
  `{index, emitted_at_ms, frame_depth, event}` envelopes (flattened
  so `jq '.type'` still works). 100 MB rotation, `Drop`-flush,
  errors swallowed so a broken sink never kills a session.
  `agent_sessions::open_or_create` auto-registers the sink when
  `HARN_EVENT_LOG_DIR` is set. Foundation for the scrubber,
  branch-replay, and jump-to-render IDE actions.
- **Branch-replay via `fork_at` (#105).** `agent_sessions::fork_at`
  forks a source session and truncates the new session's transcript
  to the first N messages, so the scrubber can rewind to a past
  event index and spawn a live sibling whose next decision diverges
  cleanly. Subscribers are not carried over — parent fanout
  consumers don't double-receive.
- **Prompt render-index registry (#106).** Thread-local
  `PROMPT_RENDER_INDICES` map from `prompt_id` → `[ordinal…]`, plus
  new `prompt_mark_rendered(prompt_id) → int` host builtin that
  pipelines call right before handing a rendered prompt to
  `llm_call`. `burin/promptConsumers` now surfaces the ordinal list
  so the IDE template gutter can jump to the next matching render
  event.
- **Tool Vault foundation: native progressive tool disclosure on Anthropic.**
  Mark individual tools with `defer_loading: true` via `tool_define`
  (or the dict form) and opt a call into progressive disclosure with a
  new `tool_search: "bm25" | "regex" | {variant, mode, always_loaded}`
  option on `llm_call` / `agent_loop`. On Claude Opus/Sonnet 4.0+ and
  Haiku 4.5+, Harn emits native `defer_loading: true` in the tool JSON
  and prepends the appropriate `tool_search_tool_{bm25,regex}_20251119`
  server tool. Schemas stay in the API prefix (so prompt caching
  remains warm) but out of the model's context until the model
  discovers them. Typical token reductions of ~85% for large tool
  catalogues. Phase 1 of the Harn Skills & Tool Vault series; see
  harn#69 for the full plan and follow-up issues.
- **Provider capability surface.** The `LlmProvider` trait gains
  `supports_defer_loading(&str) -> bool` and
  `native_tool_search_variants(&str) -> &[&str]`, letting Harn decide
  per-provider per-model whether native progressive disclosure is
  available. Anthropic implements both; OpenAI lands in harn#71.
- **Transcript events for tool search.** Anthropic `server_tool_use`
  and `tool_search_tool_result` response blocks are now parsed into
  structured `tool_search_query` and `tool_search_result` events in
  the run record — replay / eval can reconstruct which tools got
  promoted when without re-running the call.
- **Pre-flight validation.** Passing `tool_search` with every tool
  set to `defer_loading: true` errors before the API call, matching
  Anthropic's documented 400. `defer_loading` itself is type-checked
  at `tool_define` so typos fail fast.

### Breaking

- Non-Anthropic providers (or Anthropic models older than 4.0 Opus/
  Sonnet / 4.5 Haiku) error with a precise diagnostic when
  `tool_search` is requested, pointing at harn#70 for the upcoming
  client-executed fallback. This is intentional (no silent
  degradation); client fallback makes the feature provider-agnostic
  in the next phase.
- **Distributive instantiation of generic type aliases.** Applying a
  generic `type F<T> = ...` alias to a closed union now expands into a
  union of per-variant instantiations rather than leaving the union in
  a single `T` slot. Concretely, for

  ```harn
  type Action = "create" | "edit"
  type ActionContainer<T> = { action: T, process_action: fn(T) -> nil }
  ```

  the type `ActionContainer<Action>` now resolves as
  `ActionContainer<"create"> | ActionContainer<"edit">`, which lets a
  `fn("create") -> nil` handler flow into the `"create"` branch without
  running aground on the contravariance of the function parameter.
  This is the pattern TypeScript rejects in the classic
  `Array<ActionContainer<Action>>` playground example; Harn now handles
  it soundly via distribution at alias-application time in
  `crates/harn-parser/src/typechecker/inference/subtyping.rs`. No new
  syntax or keyword required — distribution is an implementation of
  existing alias-application semantics.
- **Discriminator narrowing on tagged shape unions.** A union of two or
  more dict shapes that share a literal-typed, distinct-per-variant
  field is now a *tagged shape union*. Matching on that field
  (`match obj.<tag>`) or testing it (`if obj.<tag> == "value"` /
  `else`) narrows `obj` to the matching variant inside each arm or
  branch. The discriminant is auto-detected — there is no privileged
  field name, `kind` and `type` and `op` and any other shared
  literal-typed field all work identically. Plain literal unions
  (`"pass" | "fail" | "unclear"`) gain the same exhaustive `match`
  treatment as enums.
- **Reserved keywords are now legal shape-type field names.**
  `{type: "click", x: int, y: int}` parses in type position as well
  as in dict-literal and property-access position. Closes a small
  asymmetry that previously forced workarounds for `type`-tagged
  shape unions.
- **Conformance and quickref pin the surface contract.** New
  conformance tests `shape_union_discriminator_forms` (parse +
  format invariants across `kind`, `type`, and `op` discriminants
  plus pure literal unions) and `shape_union_discriminator_narrow`
  (end-to-end narrowing in match arms and `if` branches). The
  `harn-scripting` skill autoloads `docs/llm/harn-quickref.md`,
  which now ships a "Discriminated unions & distribution" block
  with copy-paste-ready examples for all three forms.
- **Residual + post-distribution narrowing conformance.** Two new
  fixtures pin behaviour that was previously only covered by
  typechecker unit tests:
  `shape_union_not_equal_narrowing.{harn,expected}` exercises the
  residual narrow on `if obj.<tag> != "value"` (truthy branch
  narrows to the union of the other variants; else branch narrows
  to the single matched variant). `shape_union_post_distribution.
  {harn,expected}` exercises `Container<A | B>` distributing to
  `Container<A> | Container<B>` and then going through the tagged-
  shape-union discriminator-narrowing path end-to-end.
- **LSP: tagged shape union hover expands each variant.** Hovering
  on a variable typed as a tagged shape union (two-plus dict
  shapes) previously collapsed onto a single wide line. The hover
  handler in `crates/harn-lsp/src/handlers.rs` now invokes
  `format_union_shapes_expanded` (new in `symbols.rs`) to render
  each variant on its own block with field-per-line formatting,
  separated by `|` — matching the existing `format_shape_expanded`
  style used for single shapes.
- **LSP: completion of discriminator literal values inside `match`.**
  When the cursor sits in arm-pattern position of a `match obj.<tag>
  { … }` block and `obj` resolves to a tagged shape union, the
  completion list now surfaces each distinct discriminator literal
  as an `ENUM_MEMBER` item (with the matched variants for arms
  already present filtered out). Implemented via an AST walk in
  `discriminator_value_completions`; the type-alias chain is
  resolved through `resolve_type_alias_from_ast` so `m: Msg` with
  `type Msg = Ping | Pong` is treated identically to an inline
  union.
- **LSP: quick-fix to add missing `match` arms.** The typechecker
  now attaches a structured `DiagnosticDetails::NonExhaustiveMatch
  { missing: Vec<String> }` payload to non-exhaustive-match errors
  on enums, tagged shape unions, and literal unions. The LSP code-
  action provider reads it and synthesises a `WorkspaceEdit` that
  inserts one stub arm per missing variant
  (`<literal> -> { unreachable("TODO: handle <literal>") }`),
  indented to match the match body's closing brace. Marked
  `isPreferred: true` so the client surfaces it first.
- **Or-patterns in `match` arms (`pat1 | pat2 -> body`).** A single
  arm may list two or more literal alternatives separated by `|`;
  the shared body runs when any alternative matches, and each
  alternative contributes to exhaustiveness coverage independently.
  Inside the arm, the matched variable is narrowed to the *union*
  of the alternatives' single-literal narrowings — on a literal
  union this is a sub-union, on a tagged shape union it is a union
  of the matching shape variants. Guards compose naturally:
  `1 | 2 | 3 if n > 2 -> …` runs the body only when some
  alternative matched *and* the guard held. Alternatives are
  restricted to literal patterns (string, int, float, bool, nil)
  and the wildcard `_`; identifier-binding and destructuring
  alternatives are rejected with a specific diagnostic. Lowering
  mirrors the existing literal-arm shape in `crates/harn-vm/src/
  compiler/patterns.rs`, so no new opcodes were needed. Pinned by
  conformance tests `match_or_pattern` (literal-union + guard
  combinations) and `shape_union_or_pattern` (narrowing into a
  two-variant union on a tagged shape union), plus typechecker
  tests in `exhaustiveness.rs` and `narrowing.rs`. Tree-sitter
  grammar adds an `or_pattern` rule, pinned by the new
  `match_arms` corpus.

### Breaking — typechecker

- **Non-exhaustive `match` is a hard error.** A `match` that omits
  enum variants, tagged-shape-union variants, named-type union
  members, or literal-union members must add the missing arm or
  end with a wildcard `_ -> { … }` arm. `if/elif/else` chains stay
  intentionally partial; opt into exhaustiveness by ending the
  chain with `unreachable("…")`, which still flows through the
  warning-level `check_unknown_exhaustiveness` path.

### Removed

- **`auto.harn` `< 40-char` safety net (#107 follow-up).** The fallback
  that routed short inputs through `chat_reply` is gone; explanation
  intents classify as `qa` upstream and take the dedicated
  `qa_reply` path. An empty result now surfaces the real pipeline
  state honestly instead of masking bugs.

### Deferred (separate follow-up)

- **Canonical ADT surface syntax** — the planned
  `type Action = Create { x: int } | Edit { y: int }` form, with a
  unified internal `TypeExpr::Adt` representation behind it, is
  intentionally *not* in this changeset. The user-visible
  capabilities the canonical syntax was meant to deliver
  (discriminator narrowing, exhaustiveness, distributive generic
  instantiation, schema oneOf via the existing enum path) are all
  in place via tagged shape unions, legacy enums, and alias
  distribution; the surface change is additive sugar that requires
  coordinated parser/VM/fmt/LSP/tree-sitter/VS Code work and
  warrants its own PR.

### Fixed

- **Tagged shape unions with `Named`-alias members now narrow.**
  `type Ping = {kind:"ping",…}; type Msg = Ping | {kind:"pong",…}`
  previously lost discriminator narrowing: the bare-`Shape` check in
  `discriminant_field` rejected the `Named("Ping")` member on sight,
  so `match m.kind` and `if m.kind == "ping"` both degraded to the
  raw `Msg` type inside the branch. `resolve_union_shape_members`
  (new helper in
  `crates/harn-parser/src/typechecker/inference/flow.rs`) resolves
  the `Named`-alias chain in each union member before
  `discriminant_field` / `narrow_shape_union_by_tag` inspect the
  shapes. Pinned by conformance
  `shape_union_named_alias_member.{harn,expected}` and typechecker
  tests `test_match_narrows_through_named_alias_member` /
  `test_if_narrows_through_named_alias_member` in `narrowing.rs`.
- **Match-arm guard no longer consumes the match value on fail.**
  When a literal-pattern match arm's guard evaluated to false, the
  emitted bytecode over-popped and consumed the match value before
  the next arm's `Dup`, surfacing as a runtime
  "Stack underflow" once a subsequent arm ran. The guard-fail path
  now falls through to the shared trailing `Pop` (same as the
  match-fail path), matching the discipline used by dict/list
  destructuring arms. The new or-pattern lowering follows the same
  corrected shape.
- **Bare function references now carry their full `fn(...)` type.**
  Previously, a top-level (or nested) function used as a plain value
  (e.g. inside a dict literal) inferred as `None`, which collapsed to
  `nil` at the surrounding inference site. A subsequent assignment into
  a typed slot then failed with a misleading "got nil" diagnostic. The
  typechecker now falls back from `scope.get_var` to `scope.get_fn`
  when resolving bare identifiers, projecting the function signature
  into a proper `FnType { params, return_type }`.

## v0.7.16

### Fixed

- **Debugger: breakpoints on the entry script now actually stop execution.**
  `harn-dap`'s `compile_program` was calling `harn_vm::compile_source`,
  which produces a `Chunk` without a `source_file` set. Because
  `Vm::breakpoint_matches` keys its lookup on the current frame's
  `chunk.source_file`, path-keyed breakpoints from a DAP client (VS Code,
  Burin, …) could never match — only the wildcard (empty-string) set
  fired, which clients don't emit in practice. Imported modules already
  got the right tag via `compile_fn_body`; the entry chunk now gets it
  too. `test_breakpoint_stop` is tightened to demand `reason="breakpoint"`
  so the regression can't recur silently.

## v0.7.15

### Changed

- **Internal: finished splitting the remaining oversized source files
  into focused modules.** v0.7.13's `typechecker/` split continues with
  six more multi-thousand-line files, each now a directory of focused
  submodules. Public API surface is preserved through `pub(crate) use`
  re-exports in each `mod.rs`, so downstream crates and call sites are
  unchanged. Bytecode output and all conformance/unit/portal/
  tree-sitter tests are byte-for-byte identical (472/472 conformance,
  164/164 parser, 130/130 harn-cli, 16/16 tree-sitter). Every resulting
  file is under ~1,200 lines.
  - `crates/harn-parser/src/parser.rs` (3,038 lines) → `parser/`
    module split into `decls`, `error`, `expressions`, `patterns`,
    `state`, `statements`, and `types` (closes #41).
  - `crates/harn-vm/src/compiler.rs` (3,631 lines) → `compiler/`
    module split into `closures`, `concurrency`, `decls`, `error`,
    `error_handling`, `expressions`, `patterns`, `pipe`, `state`,
    `statements`, `tests`, and `yield_scan` (closes #38).
  - `crates/harn-vm/src/stdlib/workflow.rs` (2,240 lines) →
    `workflow/` module split into `artifact`, `convert`, `guards`,
    `map`, `policy`, `register`, `stage`, `tests`, and `usage`
    (closes #45).
  - `crates/harn-cli/src/commands/portal.rs` (3,070 lines) → `portal/`
    module split into `assets`, `dto`, `errors`, `handlers/`,
    `highlight`, `launch`, `llm`, `query`, `router`, `run_analysis`,
    `state`, `transcript`, and `util` (closes #40).
  - `crates/harn-cli/src/commands/check.rs` (3,505 lines) → `check/`
    module split into `bundle`, `check_cmd`, `config`, `fmt`,
    `host_capabilities`, `imports`, `lint`, `mock_host`, `outcome`,
    `preflight`, and `tests` (closes #39).
  - `crates/harn-lint/src/lib.rs` (2,652 lines) → focused modules:
    `diagnostic`, `decls`, `naming`, `harndoc`, `linter` (+
    `linter/walk`), and one file per source-aware rule under `rules/`
    (`blank_lines`, `file_header`, `import_order`, `trailing_comma`)
    (closes #43).

## v0.7.14

### Fixed

- **Lexer: multi-line `${…}` interpolation now tracks line numbers.**
  Inside a single-line string, the `${…}` expression can itself span
  multiple physical lines (e.g. `${render(\n  "a",\n  b,\n)}`). The lexer
  consumed those inner newlines without advancing `self.line`, so every
  token after such a string reported a line number that was too low —
  by the number of newlines consumed inside the interpolation. Downstream
  `missing-harndoc` lint spans pointed at the wrong declarations. Matches
  the long-standing behavior of the multi-line (`"""…"""`) string lexer,
  which already handled this correctly.
- **Formatter: doc comment between `@attr` and `pub fn` is preserved.**
  Placing `/** … */` between an attribute and its declaration (the order
  the `missing-harndoc` rule requires when both are present) used to
  drop the doc and re-emit it above the *next* top-level item. The
  formatter now emits comments in the `last_attr.span.line + 1 ..
  inner.span.line` range before recursing into the inner declaration.

## v0.7.13

### Changed

- **Anthropic provider: Claude Opus 4.7 compatibility.** The Anthropic
  request builder now recognizes Claude model generations and applies
  Opus 4.7's breaking API changes automatically:
  - Sampling parameters (`temperature`, `top_p`, `top_k`) are stripped
    from request bodies for Opus 4.7+ models (Anthropic returns HTTP 400
    on non-default values). A one-time `llm.sampling` warning surfaces
    when we drop them.
  - `thinking: {type: "enabled", budget_tokens: N}` payloads are
    transparently rewritten to `thinking: {type: "adaptive"}` for Opus
    4.7+ models (extended thinking was removed from that generation).
    Pipeline authors don't need to special-case the API change; the
    provider layer handles it and logs once per model.
  - The pre-existing prefill gate (deprecated in Claude 4.6) is now
    generation-aware: it fires for every `claude-*-4.6+` model in either
    dash (`claude-opus-4-7`) or dotted (`anthropic/claude-opus-4.7`)
    form, replacing the previous hardcoded family-name list.
- **Internal: `harn-parser` typechecker split into a `typechecker/`
  module.** The 7,782-line `typechecker.rs` is now a directory of
  focused files (`scope`, `format`, `union`, `exits`,
  `schema_inference`, `binary_ops`, and an `inference/` sub-module split
  by node-kind family). The public API is re-exported from
  `typechecker/mod.rs`, so no downstream crate needed edits. Docs-snippet
  coverage was also extended: 9 `harn` fences across `concurrency`,
  `error-handling`, `language-basics`, `language-spec`, and
  `scripting-cheatsheet` now include the helper stubs they reference so
  `harn check` passes under the stricter cross-module undefined-call
  gate added in v0.7.12.

## v0.7.12

### Added

- **Static cross-module undefined-call errors.** `harn check`,
  `harn run`, `harn bench`, and the LSP now share one recursive module
  graph built by `harn-modules`. When every import in a file resolves,
  the typechecker treats any call target that is not a builtin, local
  declaration, struct constructor, callable variable, or imported
  symbol as an error (`call target ... is not defined or imported`)
  instead of letting the VM discover it at runtime. If any import is
  unresolved, the stricter check is skipped for that file so one broken
  import does not cascade into a flood of false positives.

### Changed

- **DRY cross-module primitives.** LSP go-to-definition now walks the
  same `harn_modules::ModuleGraph` used by check/lint, instead of its
  own duplicated import-walking logic. `harn-lsp`, `harn-lint`, and the
  CLI all consume a single `harn_modules::build(...)` call per entry
  file, which transitively loads every reachable `.harn` module once.

## v0.7.11

### Added

- **DAP pause-during-run.** `pause` now interrupts a program that is
  actively executing instead of being a no-op during runs. The adapter's
  main loop interleaves VM steps with non-blocking drains of the DAP
  request channel, so `pause`, `setBreakpoints`, and `disconnect`
  arriving mid-run are serviced between steps. On `pause`, the next step
  tick stops with `reason: "pause"` without advancing the VM.
- **DAP progress events during runs.** `configurationDone` now emits a
  `progressStart` so the IDE shows a "Running…" indicator, with
  throttled `progressUpdate` ticks (roughly every 256 VM steps) carrying
  the current line. Progress is ended on every stop path (breakpoint,
  pause, exception, terminate, disconnect) so the IDE's liveness
  indicator clears cleanly.
- **`harnPing` DAP request.** Lightweight liveness check the IDE can
  send to distinguish "wedged" from "actively stepping". Responds with
  `{state, running, stopped}` derived from the current debugger state.

### Fixed

- **DAP `disconnect` no longer waits on in-flight host calls.**
  `disconnect` now cancels any pending `DapHostBridge` reverse-request
  waiters with a synthetic failure carrying `reason: "cancelled:
  disconnect"`, tears down the VM, and ends any active progress event.
  Previously, a host call in flight at disconnect time kept the script
  blocked until the 60s reverse-request timeout. Scripts now unwind
  promptly when the IDE walks away.

## v0.7.10

### Fixed

- **DAP breakpoints scoped to the requesting source file.** Previously,
  `setBreakpoints` cleared *all* breakpoints across every file before
  re-installing the new set, and the VM matched on raw line numbers
  with no regard for which source file was executing — so a breakpoint
  at line 10 of `auto.harn` would also fire when an imported library
  hit its own line 10. The DAP adapter now retains breakpoints from
  files other than the one named in the request (per spec), and the
  VM stores breakpoints in a per-file map (`set_breakpoints_for_file`)
  with a backwards-compatible wildcard form (`set_breakpoints`, empty
  key). A path-suffix fallback handles relative-vs-absolute path drift
  between IDE and runtime. Multi-file pipelines now break exactly where
  the user asked.

### Public API

- `harn_vm::Vm::set_breakpoints_for_file(file, lines)` — replace the
  breakpoint set for one source file. Existing
  `set_breakpoints(lines)` is preserved as a wildcard shorthand.

## v0.7.9

### Added

- **DAP host-call bridge (`harnHostCall` reverse request).** The
  debugger now round-trips unhandled `host_call(capability, operation,
  ...)` ops to the DAP client as reverse requests, mirroring the DAP
  `runInTerminal` pattern. On `success: true`, the adapter returns the
  body's `value` (or the whole body when `value` is absent); on
  `success: false`, it raises `VmError::Thrown(message)` so scripts can
  `try`/`catch` it. The stdin reader runs on a dedicated thread so the
  bridge can block on reply channels without starving the main message
  loop, and adapter-initiated seqs (forward responses + reverse
  requests) share one counter so every frame stays unique. Capabilities
  advertise the new `supportsHarnHostCall: true` field — clients that
  do not set the matching capability still work and simply see the
  standalone fallbacks.
- **`HostCallBridge` trait in `harn-vm`.** New public surface
  (`harn_vm::HostCallBridge`, `set_host_call_bridge`,
  `clear_host_call_bridge`) lets embedders (debug adapters, IDE hosts,
  CLI wrappers) satisfy capability/operation pairs that harn-vm itself
  does not know how to handle. `Ok(None)` falls through to the built-in
  fallbacks; `Ok(Some(_))` is the result; `Err(VmError::Thrown(_))`
  surfaces as a Harn exception. The bridge is consulted after mock
  matching and before built-in match arms, so embedders can override
  anything and equally punt on anything.
- **Standalone `workspace.project_root` / `workspace.cwd` fallbacks.**
  Pipelines call `host_call("workspace", "project_root", {})` very
  early, so the VM now answers these ops even when no embedder bridge
  is installed. `project_root` prefers `HARN_PROJECT_ROOT` and falls
  back to `std::env::current_dir()`; `cwd` always returns the current
  working directory. Keeps debug-launched scripts unblocked when the
  IDE has not wired `harnHostCall` through yet.
- **LLM-call telemetry as DAP `output` events.** The debugger enables
  harn-vm's agent trace, drains `AgentTraceEvent::LlmCall` entries
  between VM steps, and forwards them as DAP `output` events with
  `category: "telemetry"` and a JSON body (`call_id`, `model`,
  `prompt_tokens`, `completion_tokens`, `cache_tokens`, `total_ms`,
  `iteration`). Other trace kinds are skipped for now — the IDE
  consumes only LLM telemetry.
- **Cross-file go-to-definition in the LSP.** `textDocument/definition`
  now walks the document's `import` declarations, resolves each path
  with the same relative + `.harn/packages/` lookup order harn-vm
  uses, parses the imported file, builds its symbol table, and
  returns the first matching pipeline / function / variable / struct /
  enum / interface. Selective imports that name the target symbol are
  searched first so the highest-confidence hit wins.

## v0.7.8

### Added

- **Typed pipeline returns (`pipeline name() -> TypeExpr { ... }`).**
  Pipelines can now declare a return type with the same `-> TypeExpr`
  syntax as functions. The type checker validates every `return <expr>`
  against the declared type, turning the Harn→ACP/A2A boundary into a
  type-checked contract instead of relying on the host bridge as the
  only enforcement point. A new `std/acp` stdlib module ships
  canonical ACP envelope type aliases (`SessionUpdate`,
  `AgentMessageChunk`, `ToolCall`, `ToolCallUpdate`, `Plan`,
  `PipelineResult`) plus constructor helpers
  (`agent_message_chunk`/`tool_call`/`tool_call_update`/`plan`).
  Public pipelines without an explicit return type emit the
  `pipeline-return-type` lint warning as a one-release deprecation
  window; well-known entry names (`default`, `main`, `auto`, `test`)
  are exempt. Resolves
  [#31](https://github.com/burin-labs/harn/issues/31).
- **DAP `pause` request and `supports_terminate_request` capability.**
  The debugger now handles the DAP `pause` request by flipping the VM
  into step-in mode and emitting a `stopped` event when execution is
  already halted, giving IDEs a meaningful pause affordance.
  Capabilities now advertise `supports_terminate_request: true`.

### Changed

- **`cyclomatic-complexity` default bumped from 10 → 25** and made
  configurable via `[lint].complexity_threshold` in `harn.toml`. The
  old default treated any function with more than ten decision points
  as suspect, which turned the rule into the dominant lint signal in
  real Harn projects (137 of 210 warnings in `burin-code`, 65%). 25
  matches Clippy's `cognitive_complexity` default and splits the
  difference between ESLint (20) and gocyclo (30); Harn counts
  `&&`/`||` per operator, so real-world Harn functions score a notch
  higher than in tools that only count control-flow nodes. The
  diagnostic now names the `@complexity(allow)` escape hatch and the
  `harn.toml` knob. Note: the originally-proposed `harn lint --fix`
  for cyclomatic complexity was dropped after inspection — none of
  the mechanical transforms (early-return flattening, De Morgan on
  nested `if`-returns, redundant-`else` elimination) actually reduce
  the cyclomatic score, since guards and `&&`/`||` each cost 1. Those
  transforms improve cognitive complexity / nesting depth and may
  ship under a separate future lint. Resolves
  [#29](https://github.com/burin-labs/harn/issues/29).

## v0.7.7

### Added

- **Attribute / decorator surface (`@name(...)`).** Top-level
  declarations (`pipeline`, `fn`, `tool`, `struct`, `enum`, `type`,
  `interface`, `impl`) can now carry one or more attributes. The
  initial set is:
  - `@deprecated(since: "X", use: "Y")` — type-checker warning at
    every call site, with both args optional.
  - `@test` — marks a `pipeline` as a test entry point, recognized
    by `harn test conformance` alongside the legacy `test_*` naming
    convention.
  - `@complexity(allow)` — suppresses the `cyclomatic-complexity` lint
    on the attached function.
  - `@acp_tool(name: ..., kind: ..., side_effect_level: ..., ...)` —
    desugars to a runtime `tool_define(...)` call with the attached
    function bound as the handler and named args (other than `name`)
    lifted into the `annotations` dict so `ToolAnnotations` flows
    through ACP/A2A unchanged.

  Attribute arguments are restricted to literal values (strings,
  numbers, `true`/`false`/`nil`, bare identifiers) — there is no
  runtime evaluation. Unknown attribute names produce a type-checker
  warning so misspellings surface at check time. Documented in
  `spec/HARN_SPEC.md` ("Attributes" section) and the quickref.
  Resolves [#30](https://github.com/burin-labs/harn/issues/30).

## v0.7.6

### Added

- **Stdlib polish: `llm_call_safe`, `read_file_result`, `env_or`,
  `with_rate_limit`.** Four small builtins that eliminate repetitive
  ceremony in grading/bench/eval scripts. `llm_call_safe(prompt,
  system?, opts?)` is a non-throwing envelope around `llm_call`
  returning `{ok, response, error: {category, message} | nil}`, with
  `error.category` drawn from the canonical `ErrorCategory` string
  set (`"rate_limit"`, `"timeout"`, `"overloaded"`,
  `"transient_network"`, `"schema_validation"`, etc).
  `read_file_result(path)` is a non-throwing sibling of `read_file`
  returning `Result.Ok(content)` / `Result.Err(message)` and sharing
  the same content cache. `env_or(key, default)` collapses the
  `let v = env(K); if v { v } else { default }` pattern. `with_rate_limit(provider, fn, opts?)`
  acquires a sliding-window permit and retries the closure with
  exponential backoff on retryable categories (`rate_limit`,
  `overloaded`, `transient_network`, `timeout`) — composes with
  `HARN_RATE_LIMIT_<PROVIDER>` env vars and `llm_rate_limit(...)`.
  Resolves [#28](https://github.com/burin-labs/harn/issues/28).
- **`llm_mock` error injection.** `llm_mock({error: {category, message}})`
  now synthesizes a `VmError::CategorizedError` on match instead of an
  `LlmResult`, so `try { llm_call(...) }`, `error_category`,
  `llm_call_safe`'s error envelope, and `with_rate_limit`'s retry loop
  all have deterministic test coverage. Unknown category strings are
  rejected at registration time.

## v0.7.5

### Added

- **Generic inference across schema-driven builtins.** `llm_call`,
  `llm_completion`, `schema_parse`, `schema_check`, and `schema_expect`
  now carry real generic signatures keyed on a new `Schema<T>` type
  constructor. User-defined wrappers inherit the same narrowing
  without any typechecker special case:

  ```harn
  fn grade<T>(prompt: string, schema: Schema<T>) -> T {
    let r = llm_call(prompt, nil,
      {output_schema: schema, output_validation: "error",
       response_format: "json"})
    return r.data
  }

  let out: GraderOut = grade("Grade this", schema_of(GraderOut))
  // out.verdict / out.summary narrow without schema_is guards.
  ```

  The `Schema<T>` type constructor denotes a runtime schema value
  whose static shape is `T`. When a parameter is typed `Schema<T>`,
  the argument's value node (a type-alias identifier, `schema_of(T)`,
  or an inline JSON-Schema dict) binds the generic parameter,
  threading the narrowed type through the call's return type. The
  hand-rolled `extract_llm_schema_from_options` narrowing is
  removed in favor of this generic dispatch, and user generic
  functions use the same node-walking inference. Runtime
  `schema_of(T)` is unchanged. Resolves
  [#33](https://github.com/burin-labs/harn/issues/33).

## v0.7.4

### Added

- **Comprehensive variance (`in T` / `out T`).** Type parameters on
  user-defined generics may now be marked with `in` (contravariant)
  or `out` (covariant). Unannotated parameters default to
  **invariant** — strictly safer than the previous implicit
  covariance. The subtype relation is now polarity-aware: built-in
  `iter<T>` is covariant, `list<T>` and `dict<K, V>` are invariant
  (mutable), and function types are contravariant in their parameters
  and covariant in their return type. Declaration sites are checked
  too: `type Box<out T> = fn(T) -> int` is rejected because `T`
  appears in a contravariant position. Generic type aliases
  (`type Foo<T> = ...`) are now supported in the parser. See the
  spec's "Subtyping and variance" section. Resolves
  [#34](https://github.com/burin-labs/harn/issues/34).
- **`fn`-type parameter contravariance fix.** Function-type
  parameter compatibility was previously checked covariantly, which
  let `fn(int) -> R` stand in for an expected `fn(float) -> R` —
  unsound, since the caller may hand the closure a float it cannot
  receive. Parameters are now checked contravariantly per the
  variance rewrite above; `fn(float)` correctly substitutes for
  `fn(int)` but not the reverse.
- **Exhaustive narrowing on `unknown`.** The type checker now tracks
  which concrete `type_of` variants have been ruled out on each
  flow path for every `unknown`-typed variable. When control flow
  reaches a never-returning site — `unreachable()`, a `throw`, or a
  call to a user-defined function with return type `never` — the
  checker warns if the coverage set is non-empty but incomplete,
  naming the uncovered variants. Plain `return` fallthroughs are
  not exhaustiveness claims and stay silent, and a bare `throw`
  with no prior `type_of` narrowing also stays silent. Resolves
  [#27](https://github.com/burin-labs/harn/issues/27).
- **`try*` finally-pop fix.** Compiler now unconditionally pops the
  one-value-per-block leftover after a finally body, so a `finally`
  ending in a non-value statement (e.g. `x = x + 1`) no longer leaks
  a stray `nil` onto the stack of the surrounding expression. This
  was latent in `try { ... } finally { x = x + 1 }` used in
  expression position; surfaced while wiring the new `try*` operator.
- **`try* EXPR` — rethrow-into-catch operator.** Replaces the
  `try { foo() } / guard is_ok else / unwrap` boilerplate with a
  one-token prefix form. `try* EXPR` evaluates `EXPR` and, on a thrown
  error, runs every `finally` block between the rethrow site and the
  innermost catch handler exactly once before rethrowing the original
  value into that handler. On success it evaluates to `EXPR`'s value
  with no `Result` wrapping. `try*` requires an enclosing function
  (`fn`, `tool`, or `pipeline`) — using it at module top level is a
  compile error. Distinct from postfix `?` (which early-returns
  `Result.Err(...)` from a Result-returning function); use `try*` when
  you want a thrown error to land in an enclosing `try { ... } catch`
  rather than be returned as a Result. Resolves
  [#26](https://github.com/burin-labs/harn/issues/26).
- **Schema-as-type: unified `type` aliases with `output_schema` /
  `schema_*` builtins.** A `type` alias can now drive both static
  type-checking and runtime schema validation from a single source of
  truth. `schema_of(T)` lowers a type alias to a JSON-Schema dict at
  compile time, and the same alias identifier is accepted as the schema
  argument of `schema_is` / `schema_expect` / `schema_parse` /
  `schema_check` / `is_type` / `json_validate`, and as the value of
  `output_schema:` in an `llm_call` options dict. Narrowing on
  `schema_is(x, T)` refines `x` to `T` in the truthy branch. The type
  grammar now admits string- and int-literal types in unions
  (`"pass" | "fail" | "unclear"`, `0 | 1 | 2`), emitted as canonical
  `{type, enum}` JSON Schema so schemas are compatible with both
  structured-output validators and ACP `ToolAnnotations.args`. Resolves
  [#25](https://github.com/burin-labs/harn/issues/25). See
  `docs/src/migrations/schema-as-type.md` for the migration guide.
- **`harn check` workspace manifest (`[workspace].pipelines`).** The CLI now
  walks upward from a target file (stopping at the first `.git` boundary) to
  find the nearest `harn.toml` and honors a new `[workspace]` section. Run
  `harn check --workspace` to type-check every `.harn` file under the listed
  pipeline roots in a single invocation without threading per-file
  `--host-capabilities` flags. See `spec/HARN_SPEC.md` → "Workspace manifest
  (`harn.toml`)" and `docs/src/cli-reference.md` for the schema.
- **Preflight diagnostics separated from type errors.** Preflight diagnostics
  from `harn check` are now reported under a distinct `preflight` category so
  IDE filters and CI log scrapers can route them separately from type-checker
  output. Two new knobs control them: `[check].preflight_severity = "error"
  | "warning" | "off"` (overridable with the new `--preflight <severity>`
  flag), and `[check].preflight_allow = ["capability.operation", ...]` which
  accepts exact matches, `capability.*` wildcards, bare capability names, or
  a blanket `*`. The existing `--host-capabilities` flag continues to work as
  a per-invocation override of `[check].host_capabilities_path`. Resolves
  [#24](https://github.com/burin-labs/harn/issues/24).

### Breaking

- **Unannotated user generics are now invariant by default**
  instead of implicitly covariant. Code that relied on
  `MyBox<int>` flowing into `MyBox<float>` must add an explicit
  `out T` annotation to the declaration (and ensure `T` only
  appears in covariant positions). See
  [#34](https://github.com/burin-labs/harn/issues/34).
- **`list<T>` and `dict<K, V>` are now invariant.** `list<int>` no
  longer flows into `list<float>`, and `dict<string, int>` no
  longer flows into `dict<string, float>`. Mutable containers
  cannot be soundly covariant on writes; use `iter<T>` for
  read-only sequences that still need element-type widening.
- **`fn`-type parameters are now contravariant.** `fn(int)` no
  longer satisfies an expected `fn(float)`. The reverse direction
  (`fn(float)` standing in for `fn(int)`) is the new accepted form.

## v0.7.3

### Added

- **`any` is now a true top type; `unknown` added as the safe top.**
  Previously `any` behaved like a plain named type that only matched
  itself — assigning `nil` to an `any`-typed slot raised
  `'x' declared as any, but assigned nil`. With this release, `any`
  accepts every concrete type and flows back out to every concrete
  type with no narrowing required (the explicit escape hatch). A new
  `unknown` type fills the TypeScript-style "safe top" role: every
  value is assignable to `unknown`, but `unknown` is not assignable
  back to any concrete type without narrowing via
  `type_of(x) == "..."` or `schema_is(x, Shape)`. `unknown` is the
  preferred annotation for values arriving from untrusted boundaries
  (parsed JSON, LLM responses, dynamic dicts). `unknown` interoperates
  with `any` in both directions. See the new `### The any type` and
  `### The unknown type` sections in `spec/HARN_SPEC.md`, and the
  `Typing: any vs unknown vs no annotation` block in
  `docs/llm/harn-quickref.md`. Conformance coverage lives in
  `conformance/tests/any_top_type.*`, `unknown_safe_top.*`,
  `unknown_requires_narrowing.*`, and `unknown_narrowing.*`.

## v0.7.2

### Added

- **`try/catch/finally` as an expression.** `let v = try { work() } catch (e)
  { fallback }` now binds directly — the form evaluates to the try body's
  tail value on success or the catch handler's tail value on a caught throw,
  without routing through `Result` helpers. A trailing `finally { ... }`
  runs once for side-effect only and does not contribute a value. Typed
  catches (`catch (e: AppError) { ... }`) still rethrow past the expression
  when the thrown error's type does not match the filter, so the `let`
  binding is never established. The bare `try { body }` form continues to
  wrap in `Result<T, E>` — only adding `catch` or `finally` switches to the
  handled-expression shape. See `docs/src/error-handling.md` and
  `spec/HARN_SPEC.md`.
- **Tree-sitter grammar: `try` is now a unified expression rule.** The
  grammar previously exposed `try_catch_statement` and `try_expression` as
  separate rules; both forms — statement-position `try/catch/finally` and
  expression-position `try`, `try/catch`, `try/finally`, and
  `try/catch/finally` — are now modeled as one `try_expression` rule with
  optional `catch` and `finally` clauses. This removes a parse-time
  split that no longer matched runtime semantics and keeps the grammar
  aligned with the parser.

### Fixed

- **`finally` runs exactly once per control-flow path.** A longstanding
  compiler bug pre-ran pending `finally` bodies when lowering `throw`,
  and then ran them *again* after a local `catch` finished — so on the
  caught-throw path every `finally` fired twice, and when a catch body
  itself rethrew, the outer `finally` fired three times. The compiler
  now installs a `CatchBarrier` in the pending-finally stack for each
  active `try/catch` handler: throws lowered inside that handler's try
  body pre-run only the finallys they will actually unwind past, while
  `return` / `break` / `continue` continue to run every pending finally
  up to their target. The `compile_rethrow_with_finally` helper that
  double-emitted the finally has been removed in favor of a plain
  rethrow on the catch-escape path. Covered end-to-end by the new
  `conformance/tests/finally_runs_once.*` fixture.

## v0.7.1

### Added

- **Prompt template engine v2.** `render(...)` / `render_prompt(...)` /
  the `template.render` host capability now support `{{ if }} / {{ elif }} /
  {{ else }} / {{ end }}` branching, `{{ for item in xs }} ... {{ else }} ...
  {{ end }}` loops with `{{ loop.index }}`, `.index0`, `.first`, `.last`,
  `.length`, dict iteration (`{{ for k, v in dict }}`), nested path access
  (`{{ user.tags[0] }}`), full boolean and comparison operators in
  conditions, a filter pipeline (`{{ name | upper | default: "anon" }}`)
  with built-in filters (`upper`, `lower`, `title`, `trim`, `capitalize`,
  `length`, `first`, `last`, `reverse`, `join`, `default`, `json`,
  `indent`, `lines`, `escape_md`, `replace`), `{{ include "partial.prompt"
  }}` with optional `with { ... }` scoping and cycle detection,
  `{{# comments #}}`, `{{ raw }} ... {{ endraw }}` verbatim blocks, and
  `{{- trim whitespace -}}` markers. Existing templates remain
  byte-for-byte compatible — pre-v2 `{{ name }}` and `{{ if key }} ...
  {{ end }}` syntax is a strict subset. The duplicate
  `replace()`-based implementation that used to back the
  `("template", "render")` host capability has been removed; host-call
  and script rendering now share the single canonical engine. See
  `docs/src/prompt-templating.md` and
  `docs/src/migrations/template-engine-v2.md`.
- **Preflight template-parse validation.** `harn check` now parses every
  template referenced by a literal `render(...)` or `render_prompt(...)`
  argument and surfaces syntax errors (e.g. unterminated `{{ for }}` block)
  before the pipeline runs.
- **VS Code: `.harn.prompt` / `.prompt` syntax highlighting.** A new
  TextMate grammar ships with the extension.
- **`tool_ref(name)` and `tool_def(name)` stdlib builtins.** Resolve a
  tool-name reference against the currently-bound tool registry, so
  prompt strings and host-bridge code can interpolate canonical tool
  names (and descriptions) instead of hand-typed string literals that
  silently rot on rename. Both builtins throw with the list of
  registered tools when the name is unknown or no registry is bound.
- **`tool_bind(registry)` stdlib builtin.** Installs a tool registry as
  the current thread's active binding, so `tool_ref` / `tool_def` can
  resolve names without plumbing the registry through every call site.
  Pass `nil` to clear the binding. `agent_loop` installs its own tools
  registry automatically for the duration of the run.

## v0.7.0

**First-class sessions.** The old `transcript_policy` config pattern is gone.
Session lifecycle — open, reset, fork, trim, compact, inject, snapshot, close —
is now driven by explicit imperative builtins. Unknown inputs are hard errors
instead of silent no-ops. The session store is the single source of truth for
session-scoped VM state: transcript history and closure subscribers both live
on the session now.

This is a semver-minor in the 0.x series. It is a breaking change: pipelines
that relied on `transcript_policy` dict semantics, `transcript_id` /
`transcript_metadata` on `llm_call` options, or the opaque `transcript`
option key must migrate.

### Breaking

- **`transcript_policy` removed everywhere it lived.** Deleted on workflow
  graph nodes, worker carry policies, and the `TranscriptPolicy` struct
  itself. The auto-compaction fields that used to live on it (`auto_compact`,
  `compact_threshold`, `tool_output_max_chars`, `compact_strategy`,
  `hard_limit_tokens`, `hard_limit_strategy`) moved to a dedicated
  `AutoCompactPolicy` struct under `node.auto_compact`. The `visibility`
  field was split out to a direct `node.output_visibility: string | nil`.
- **`workflow_set_transcript_policy` removed.** Replaced by
  `workflow_set_auto_compact` and `workflow_set_output_visibility`.
- **`mode: "reset" | "fork"` lifecycle dict is gone.** Call
  `agent_session_reset(id)` or `agent_session_fork(src)` explicitly.
- **`transcript` option key on `llm_call` / `agent_loop` now hard-errors.**
  Pass `session_id: id` — the loop loads prior messages from the session
  store as a prefix, and persists the final transcript back on exit.
- **`LlmCallOptions::transcript_id` and `transcript_metadata` removed.**
  Session id subsumes both. `transcript_summary` stays (per-call summary
  injection for mid-loop compaction output).
- **`CLOSURE_SUBSCRIBERS` thread-local in `agent_events.rs` removed.**
  Subscribers now live on `SessionState.subscribers` in
  `crate::agent_sessions`. `agent_subscribe(id, cb)` opens the session
  lazily and appends. `clear_session_sinks` no longer evicts the session
  itself — it only clears external ACP-style sinks.
- **`execute_stage_node` no longer takes a `transcript: Option<VmValue>`
  param.** Stages read prior transcripts from the session store instead,
  via the stage's resolved `session_id`.
- **Unknown `agent_session_compact` option keys, a missing `role` on
  `agent_session_inject`, a negative `keep_last`, and lifecycle verbs
  called against an unknown id all raise** `VmError::Thrown`. Previously
  many of these were silent pass-throughs.
- **Workflow `input_contract.require_transcript` now checks the session
  store** (via the stage's `model_policy.session_id`) rather than the
  ambient threaded transcript dict.

### Added

- **Ten new builtins** in `crate::stdlib::agent_sessions`:
  `agent_session_open`, `_exists`, `_length`, `_snapshot`, `_reset`,
  `_fork`, `_close`, `_trim`, `_compact`, `_inject`. Fully documented in
  `docs/src/sessions.md`, exercised by new conformance tests
  `agent_sessions_basic.harn` and `agent_sessions_fork.harn`, and
  covered by 12 Rust integration tests in
  `crates/harn-vm/tests/agent_sessions.rs`.
- **`workflow_set_auto_compact(graph, node_id, policy)`** and
  **`workflow_set_output_visibility(graph, node_id, visibility)`** replace
  the single `workflow_set_transcript_policy`.
- **`crate::agent_sessions` module** — public per-thread session store with
  LRU eviction (default 128 sessions per VM), subscriber fanout, transcript
  round-trip for the agent loop.
- **`redact_transcript_visibility`** lifted to a public helper in
  `crate::orchestration::policy`, reusable from workflow stages and any
  embedder that wants to filter a transcript by visibility.

### Changed

- `agent_loop` with a caller-supplied `session_id` now loads prior
  messages from the session store as a prefix before running, and
  persists the final transcript back on exit. Calls without a
  `session_id` (or with an empty string) mint an anonymous id and do
  not touch the store — preserving the one-shot call shape.
- Workflow stage execution derives its `session_id` from the node's
  `model_policy.session_id`; two stages sharing an id share a
  conversation automatically.

## v0.6.3

Maintenance release focused on **comment hygiene and dependency freshness**.
No user-visible language or runtime changes — behavior, builtins, and the
conformance suite are unchanged (still 419 tests, 546 Rust tests).

### Changed

- **Comment cleanup across the workspace** — 114 files, ~2,100 net lines
  removed. Pruned section-header banners, drift narration from prior
  refactors, step-by-step breadcrumbs that paraphrased function bodies, and
  doc comments that just restated signatures. Preserved comments that document
  non-obvious invariants, protocol/spec compliance (JSON-RPC, MCP, ACP),
  provider-specific quirks (Anthropic, OpenAI, Google, Ollama, Together,
  OpenRouter), and real footguns.
- **`rand` upgraded from 0.8 to 0.9** — migrated deprecated
  `thread_rng`/`gen`/`gen_range` call sites to the renamed `rng`/`random`/
  `random_range` APIs.
- **`sha2` upgraded from 0.10 to 0.11** — unified the `sha2::*` and `md5`
  hash builtins under a single hex-encoding macro now that both pin
  `digest 0.11`.
- **`futures` pin relaxed** from the exact `0.3.32` to the semver-compatible
  `0.3`, matching the rest of the workspace's version-range style.
- `cargo update` brought patch bumps for tokio, rustls, hashbrown, indexmap,
  wasm-bindgen, and several transitive deps.

## v0.6.2

Polish patch focused on **agent-loop correctness and error-handling
depth**. Restructures error classification end-to-end (new
`ErrorCategory` variants, HTTP-status mapping fixes, category-first
retry classifier, RFC-compliant retry-after parsing), fixes several
silent-failure modes in the agent loop, hardens the streaming
transport against pathological slow-drip providers, and unifies a
handful of CLI and observability rough edges. Conformance suite goes
from 418 → 419 tests; Rust tests from 530 → 546.

### Breaking

- **`ErrorCategory` gains 4 variants** — `Overloaded`, `ServerError`,
  `TransientNetwork`, `SchemaValidation`. Non-exhaustive matches on
  `ErrorCategory` at the FFI/host-consumer boundary must handle the
  new variants (or add a wildcard arm). In-tree exhaustive sites were
  updated in this commit.
- **HTTP status → category mapping corrected.** 503 is now
  `Overloaded` (not `RateLimit` — 503 is an overload/shedding signal,
  not a quota hit). 500 and 502 are now `ServerError` (were falling
  through to `Generic`). 529 is `Overloaded`. 504 stays `Timeout`.
  Hosts that pattern-match on `rate_limit` will no longer see 503s
  there; match on `overloaded` or use `ErrorCategory::is_transient()`
  for a retry decision.
- **Anthropic `overloaded_error` string matches `Overloaded`**, not
  `RateLimit`. Same rationale as the status-code fix.
- **`agent_loop` terminal `status` distinguishes budget exhaustion.**
  When the loop completes `max_iterations` without any natural break,
  `status` is now `"budget_exhausted"` (previously the same `"done"`
  used for natural termination). Daemon loops in the same condition
  report `"budget_exhausted"` instead of being silently relabeled as
  `"idle"`. The conformance `agent_daemon_mode` fixture was updated
  to assert the new shape; host consumers that keyed off `"done"` to
  detect "agent is finished" should add `"budget_exhausted"` to the
  list (the loop ran out of rope, not out of work).

### Added

- **`ErrorCategory::is_transient()`** — authoritative retry-worthy
  predicate. Returns true for `Timeout | RateLimit | Overloaded |
  ServerError | TransientNetwork`.
- **`idle_watchdog_attempts` agent_loop option** — opt-in watchdog
  that terminates a daemon with `status = "watchdog"` after N
  consecutive idle ticks returning no wake reason. Guards against a
  misconfigured daemon (bridge never signals, no timer, no watch
  paths) hanging the session silently.
- **Three internal `AgentEvent` variants** — `BudgetExhausted`,
  `LoopStuck`, `DaemonWatchdogTripped`. Hosts subscribing to the
  event stream get parity with other loop-terminal signals.
- **`cache_hit` boolean in provider-response transcript entries** so
  consumers don't reverse-engineer it from `cache_read_tokens`.
- **RFC 7231 HTTP-date support in retry-after parsing.** The previous
  implementation only handled integer-seconds form and silently
  ignored the date form that major providers emit. Numeric seconds
  are clamped to `[0, 60_000]` ms so a misbehaving provider asking
  for a 10-minute sleep doesn't freeze the caller.
- **Streaming overall deadline.** `vm_stream_llm` now enforces a
  30-minute default overall budget (or the caller's `timeout`) in
  addition to the per-chunk idle timeout, so a provider dribbling
  bytes just under the idle threshold can't hold a stream open
  forever.
- **16 new unit tests** for retry classification and retry-after
  parsing; one new conformance case (`agent_budget_exhausted`).

### Fixed

- **Partial tool-call parse-error feedback was silently swallowed**
  when a batch mixed valid and malformed calls. The feedback gate
  was `calls.is_empty() && !tool_parse_errors.is_empty()`; it is now
  `!tool_parse_errors.is_empty()` with a clarifying note that the
  other calls in the turn dispatched successfully. Previously the
  model saw an apparent random failure to follow instructions.
- **`-e` eval leaked / raced on the temp file.** Fixed path was
  `$TMPDIR/__harn_eval__.harn`, so concurrent invocations clobbered
  each other and a panic in `run_file` left the file behind. Now
  uses `tempfile::NamedTempFile` with Drop-guarded cleanup.
- **`retry-after: <seconds>` with awkward type ascription cleaned up**
  (stylistic; no behavior change on that path).
- **`stop_after_successful_tools` unknown names are now flagged.** A
  warning names the unknown tool(s) at loop start; the option is
  still tolerated (forward-compat) but the user sees why their stop
  condition never fires.
- **Schema-validation error message now includes an actionable hint.**
  If `schema_retries` was 0, the error points at the option; if it
  was > 0 and got exhausted, the error says so plainly. The error is
  also now a `CategorizedError { category: SchemaValidation, .. }`
  rather than an opaque `Thrown(String)`.

### Changed

- **`is_retryable_llm_error` is category-first.** Structured
  `CategorizedError`s route through `is_transient()`. String-shaped
  errors first consult the shared `classify_error_message` machinery
  in `value.rs` so HTTP status codes and well-known provider
  identifiers are interpreted consistently with the rest of the VM,
  then fall back to a small substring list for shapes that carry no
  status (network failure phrases).
- **Micro-allocations swept:** `.to_string_lossy().to_string()` →
  `.to_string_lossy().into_owned()` across 14 files. Identical
  semantics, one fewer allocation on the owned variant.

### Internal

- **Dead-code/lint sweep.** The `#[allow(clippy::
  arc_with_non_send_sync)]` in `stdlib/concurrency.rs` gains a
  why-comment anchoring it to the documented single-threaded
  LocalSet invariant. The `MultiSink::handle_event` clone-then-
  iterate pattern is now documented as deliberate deadlock
  avoidance rather than an obvious "optimization" candidate.
- **Docs reconciled.** `docs/src/llm-and-agents.md` now lists the
  full `status` state space and documents the new
  `idle_watchdog_attempts` option.

Rust tests: harn-vm lib 546/546, harn-cli 124/124.
Conformance: 419/419.

## v0.6.1

Patch release. Completes the WS-6 agent/mod.rs modularization started in
the 0.6.0 line: `llm/agent/mod.rs` was a 1734-LOC hub carrying most of
the per-iteration turn loop inline. This release finishes the split
along phase seams so the orchestrator reads top-to-bottom as four named
phases.

### Internal

- **`llm/agent/mod.rs` is now a ~260-LOC thin orchestrator.** The turn
  loop body expands to four phase calls — `turn_preflight`, `llm_call`,
  `tool_dispatch`, `post_turn` — with an `IterationOutcome::{Continue,
  Break}` match returned by `post_turn` to drive outer-loop control
  flow.
- **`agent/llm_call.rs`** (new, ~410 LOC) owns the provider call,
  tagged-prose parsing, parse/protocol/sentinel feedback injection, and
  ledger-tool interception.
- **`agent/tool_dispatch.rs`** (new, ~840 LOC) owns the assistant-turn
  history append, read-only parallel pre-fetch, and the per-tool
  dispatch pipeline (parse-error rejection, policy enforcement,
  declarative + host approval via `session/request_permission`,
  pre/post hooks, arg validation, loop-detect, replay/cached/fresh
  dispatch, tracing spans, `ToolCall` / `ToolCallUpdate` events,
  transcript events, tool-result message append).
- **`agent/post_turn.rs`** (new, ~490 LOC) owns both the tool-call
  post-processing path (finish_step_messages, consecutive_single_tool,
  successful_tools_used, `TurnEnd` emit, `stop_after_successful_tools`,
  optional `post_turn_callback`, auto-compaction, parse_error feedback,
  sentinel_hit break) and the text-only path (assistant-history append,
  sentinel break, parse_error continue, daemon idle-wait with
  message/resume/watch/timer wake sources, max_nudges stuck detection,
  action-turn nudge).
- **Dead code swept.** `ToolDispatchResult.rejection_followups` was
  never pushed to; dropped the field and its dead-branch guard in the
  orchestrator.

No behavior change: harn-vm lib 530/530, harn-cli 124/124 green at
every commit.

## v0.6.0

Major release that establishes Harn's **lazy iterator protocol** as a
first-class language feature, completes the **coding-agnostic agent
substrate** the 0.5 series had been converging toward, and finishes a
sweeping internal **modularization pass** across the VM, tools, ACP, and
agent crates. Doc-comment syntax migrates from `///` to `/** ... */`
canonically, and the formatter + linter gain a broad set of autofixes
that align code with the new conventions automatically.

### Added

- **Lazy iterator protocol (`Iter<T>`)** — new `VmValue::Iter` variant with
  a single-pass iteration contract, type-checked `Iter<T>` generics, and
  a full combinator surface:
  - **Sources**: `range(start, stop, step?)` builtin, `Range` values now
    implement the iterator source protocol, `.iter()` on collections.
  - **Transformers**: `map`, `filter`, `flat_map`, `take`, `skip`,
    `take_while`, `skip_while`, `zip`, `enumerate`, `chain`, `chunks`,
    `windows`.
  - **Sinks**: `collect`, `reduce`, `fold`, `sum`, `count`, `min`, `max`,
    `any`, `all`, `first`, `last`, `for_each`, `print`.
  - Conformance coverage across sources, snapshot semantics, single-pass
    exhaustion, and streaming print.
  - Python-style `for` iteration and the new inclusive `to` keyword with
    optional trailing `exclusive` replace the old `thru` / `upto` pair.
- **`VmValue::Pair<K,V>` with for-loop destructuring** — `for (k, v) in
  dict` and `for (i, x) in iter.enumerate()` both desugar through a
  first-class pair value that type-checks end-to-end.
- **`eager-collection-conversion` lint** — with autofix. Flags
  `to_list`/`to_dict`/`to_set` calls on lazy iterators whose result is
  immediately re-iterated, steering code toward the streaming form.
- **Formatter / linter autofixes** — six new `harn lint --fix` rules now
  cover: trailing commas, import ordering, blank lines between
  top-level items, optional file-header banners, legacy `///`
  doc comments, and eager collection conversion. The formatter
  canonicalizes section-header comment blocks and enforces blank lines
  between top-level items.
- **`harn.toml` project config** — the CLI now walks upward (bounded at
  git roots) to locate a project manifest and applies its `fmt` / `lint`
  options. Both `snake_case` and `kebab-case` keys are accepted.
- **Canonical doc-comment syntax** — `/** ... */` is now the canonical
  harndoc form. The lexer tags `///` and `/**` as distinct tokens, the
  formatter and a `legacy-doc-comment` lint autofix migrate existing
  code, and `missing-harndoc` now requires the `/**` form.

### Changed

- **Agent substrate is coding-agnostic.** The VM core no longer carries
  coding-specific knowledge; the agent loop communicates through a
  `ToolAnnotations` + `AgentEvent` event stream, replacing the earlier
  ad-hoc callback hooks. The ACP server now speaks canonical
  `SessionUpdate` variants end-to-end and the legacy custom `tool/*`
  bridge methods are retired.
- **Event substrate hardening.** Session event sinks use RAII for
  deterministic lifecycle, subscriber errors are logged instead of
  silently dropped, and the happy/sad paths are covered by new tests.
- **Inclusive range syntax.** `a to b` is inclusive; add a trailing
  `exclusive` keyword for half-open ranges. The older `thru` / `upto`
  forms are removed from the lexer, parser, spec, and grammar.
- **Parser and runtime error messages** — 10–15 high-frequency
  diagnostics were tightened for clarity and actionability.
- **Internal modularization.** Large single-file modules were split
  into focused submodules without changing the public surface: `agent`
  (helpers, state, finalize, turn_preflight, tests), `tools` (parse,
  handle_local, ts_value_parser, tests), `helpers` (options),
  `orchestration` (tests), `policy` (types), `acp` (events, io,
  builtins, execute).

### Fixed

- **`VmRange` overflow hardening.** Range boundaries near `i64::MAX` /
  `i64::MIN` no longer panic.
- **No `RefCell` borrow is held across iterator await points**,
  eliminating a class of runtime borrow-panic regressions that could
  trigger under concurrent iterator sinks.
- **Formatter / conformance fixes** — a handful of pre-existing
  formatter and conformance bugs surfaced by the iterator and
  agent-substrate work are resolved.

### Docs / grammar

- Tree-sitter artifacts regenerated, `harn-keywords.js` synced from the
  live lexer + stdlib, and the language spec + quickref updated to
  describe the iterator protocol, the new range syntax, canonical
  doc-comment form, and the agent-substrate event model.

## v0.5 series (0.5.0 – 0.5.83)

The 0.5 line was Harn's "language and runtime fill-in" phase. Grouped
themes (see `git log` for the per-patch detail that previously lived
here):

- **Language:** generics foundations (generic structs, enums, interface
  associated types), strict types mode with schema-aware `llm_call`
  inference, exhaustive match with guards, `defer`, unified `parallel`
  syntax (`parallel each` / `parallel settle` / `parallel N`), nil-aware
  `??` inference, destructuring with defaults, rest parameters, raw
  string literals, triple-quoted interpolation, first-class `**`
  exponentiation, `never` bottom type + `unreachable()`, typed `catch`
  variables, stricter arithmetic typing, dict `+` merge, `string * int`
  repetition, type narrowing via `type_of`.
- **Orchestration runtime:** delegated workers (`spawn_agent`,
  `send_input`, `wait_agent`, `close_agent`, `list_agents`), worker
  lifecycle events and lineage, delegated workflow stages, workflow
  retry/backoff, stage-level timeouts, `ToolApprovalPolicy` as a
  load-bearing gating primitive, `agent_loop` turn policies,
  `post_turn_callback`, `require_successful_tools`,
  `stop_after_successful_tools`, per-worker permission scoping,
  parallel workflow map execution, daemon lifecycle persistence.
- **LLM surface:** `provider: "auto"` routing, `schema_retries` +
  `schema_retry_nudge`, `llm_retries` default of 2, `llm_usage()`,
  reasoning-content support, silent-completion detection, configurable
  mock LLM, structured output extraction, `llm_mock`, append-only
  transcript event stream, `transcript_stats`,
  `transcript_events_by_kind`, `user_visible` flag on bridge
  notifications, Ollama `think: false` default, Gemma `tool_code:`
  parser fallback, text-mode tagged-protocol hardening (heredocs, bare
  calls, angle-wrapped calls).
- **Tooling:** autofix infrastructure and the first wave of
  `harn lint --fix` rules, LSP formatting + code actions, inlay hints,
  type-aware dot completions, VS Code debugger + snippets, project
  templates (`harn new`), `harn bench` / `harn viz`, OpenTelemetry
  export behind the `otel` feature flag, Dependabot, portal build
  verification wired into `make all`.
- **Protocols:** ACP session lifecycle, MCP server at protocol version
  `2025-11-25`, A2A server at `v1.0.0`, `jsonrpc` helper module,
  machine-readable host contracts (`harn contracts ...`), explicit
  runtime path builtins (`execution_root`, etc.).
- **Schema / validation:** unified runtime schema engine,
  `schema_is` / `schema_check` / `schema_parse`, `std/schema` module,
  tool declarations exposing JSON Schema metadata, `untyped-dict-access`
  lint.
- **Stdlib additions:** `yaml_parse` / `yaml_stringify`, statistical
  and vector helpers in `std/math`, `regex_replace_all` alias,
  `eval_metric` / `eval_metrics`, `md5`, structured agent trace events,
  eval suite manifests + `harn eval`.
- **Fixes and quality:** runtime hot-path allocation reductions,
  release-mode optimization tightening, formatter grouping
  preservation, LSP stability fixes, conformance regex error matching
  and glob filters, diagnostic flakiness, workflow retry data
  preservation, nested agent loop permission ceilings.

## v0.4 series (0.4.5 – 0.4.32)

The 0.4 line established Harn's core language and runtime:

- **Language:** `Result` with `Ok` / `Err`, postfix `?`, `impl` blocks,
  interfaces with implicit satisfaction and `where T: Interface`
  constraints, runtime shape validation, try-expressions, regex capture
  groups with named groups, spread in calls, default function
  arguments, `finally`, `select`, native metadata builtins.
- **Providers / LLM:** data-driven `providers.toml`, LLM introspection
  builtins (`llm_resolve_model`, `llm_infer_provider`, `llm_model_tier`,
  `llm_healthcheck`, `llm_providers`, `llm_config`), native VM-owned
  `llm_call` replacing the bridge path.
- **Orchestration:** eval suite manifests (`eval_suite_manifest`,
  `eval_suite_run`, `harn eval`), host artifact helpers for workspace
  files, snapshots, selections, command results, verification output,
  unified diffs, git diffs, review items, accept/reject decisions.
- **Code quality:** the initial modularization pass that split the
  monolithic `stdlib.rs`, `llm.rs`, `harn-lsp/main.rs`, and
  `harn-cli/main.rs` into focused submodules.
