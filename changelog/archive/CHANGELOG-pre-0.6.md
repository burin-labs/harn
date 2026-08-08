# Pre-0.6 Changelog

This archive keeps the condensed pre-v0.6 Harn release history. Harn had no
external users before 0.6.0, so these entries preserve series-level highlights;
consult `git log` for granular per-patch archaeology.

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
