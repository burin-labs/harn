---
name: harn-orchestration
short: Agent loops, tool middleware, handoffs, and orchestration patterns.
description: Use for Harn orchestration workflows, agent_loop usage, tool middleware, and handoff design.
when_to_use: Use when building or reviewing agent orchestration, persona handoffs, tool dispatch, or runtime control flow.
---

# Harn orchestration

Use this skill when designing or changing workflows that coordinate agents, tools, sessions, approvals, and host capabilities.

Pair it with [[harn-agent]] for autonomous execution and [[harn-tracing]] for transcript and replay contracts.

## Start here

- Use `docs/llm/harn-quickref.md` for `agent_loop`, `llm_call`, streams, and concurrency.
- Use `docs/llm/harn-triggers-quickref.md` for trigger manifests and route audits.
- Runtime orchestration code lives under `crates/harn-vm/src/orchestration/`.
- CLI workflow entry points live under `crates/harn-cli/src/commands/`.
- Host capability contracts cross the VM and host bridge.
- Persona and workflow examples are useful only when they remain executable.
- Prefer existing orchestration primitives before adding Rust-only control flow.
- Keep user-visible workflows testable without live credentials.

## The abstraction ladder (pick the lowest rung)

- `llm_call` = **one request** < `agent_loop` = **one goal** (one transcript,
  run to completion) < `workflow` = **more than one goal, attempt, or model**.
- **Never hand-write a `while` around `llm_call`.** A single goal that needs
  several tool round-trips is `agent_loop`, not a hand-rolled loop.
- `agent_preset(kind, options?)` **builds `agent_loop` options** (fill-nil
  packs) — it is NOT a tier. You still call `agent_loop` with its result.
- Full guidance: `docs/src/concepts/abstraction-ladder.md` (the placement
  contract table names the canonical home for every cross-cutting mechanism).

## Core surfaces

- `agent_loop` drives iterative LLM/tool workflows.
- `llm_call` and `llm_stream_call` perform direct model calls.
- `models:` / `ladder:` on `llm_call` and `agent_loop` is a cheap-first
  fallback that advances **only on transport-class failures** (429/5xx/timeout),
  never on schema failures; named ladders resolve from catalog `[model_ladders.*]`
  rows. It is mutually exclusive with explicit `model:`/`routing:`.
- `agent_edit_tools(registry?, opts?)` (`std/agent/host_tools`) is the canonical
  default mutation toolset — `write_file` / `edit_file` / `create_directory` /
  `delete_path`. Customize through the existing tool-middleware seams; do not
  re-invent per-agent mutation tools.
- Tool middleware should preserve structured tool names and arguments.
- `agent_spawn` and handoffs should carry explicit session context.
- Hooks should return typed decisions, not prose protocols.
- Triggers connect external events to Harn workflows.
- Connectors normalize external payloads before script code sees them.
- Daemon mode should keep restart and replay behavior clear.
- Receipts should describe work done and decisions made.
- Mutation sessions belong at the host boundary.

## Workflow retry, verification, and sugar

- Stage `retry_policy: {max_attempts, feedback}` turns a blind retry into a
  repair loop; `feedback: true` threads the prior attempt's findings into the
  next task. A `repair_prompt_builder` closure takes full control and receives
  `{task, attempt, findings, verification, error, prior_text, stage}`.
- **fn-verify**: a stage's `verify` may be a closure returning a bool or
  `{ok, findings?}`; a failing verdict forces the retry-eligible `failed` branch
  and threads findings into the repair prompt.
- `workflow_stages(spec)` (`std/workflow/patterns`) is pure sugar for a linear
  stage graph — byte-identical to the hand-authored `{entry, nodes, edges}`.
- `workflow_run_repair(config)` (`std/workflow/repair`) runs the one-node
  run→validate→repair loop for you (owns no loop of its own).
- **Inline executor**: pass `executor: { ctx -> ... }` to `workflow_run_repair`
  (or set `executor` on any stage node) to run a caller-supplied closure as the
  stage leaf *instead of* spawning a delegated worker — the hook for a bespoke
  in-process agent loop. The closure receives
  `{task, attempt, prior_findings, prior_verification, prior_text, artifacts}`
  and returns `{result | text, artifacts?, transcript?, verification?}`; verify,
  retry, feedback threading, and sole-writer attempt recording run around it
  unchanged. Omit it to keep the default delegated-worker leaf.
- JSON-salvage helpers live in `std/llm/safe`: `strip_think_blocks`,
  `strip_code_fences`, `extract_first_json_object`, `extract_first_json_value`,
  `parse_first_json`.

## Placement contract: where cross-cutting mechanisms live

Import the home module instead of re-deriving the behavior inline in a loop body:

- Completion gate ("are we done?") → `std/agent/judge` (`agent_completion_gate`).
- Pace / budget governors → `std/agent/governors` (`with_governance`).
- Progress / stall detectors (unified) → `std/agent/stall`
  (`agent_stall_initial_state` + `agent_stall_observe_tool_calls`).
- Tool-surface narrowing (lanes) → `std/agent/lanes` (`lane_policy`).
- Prompt overlays (data-driven nudges) → `std/agent/overlays` (`overlay_policy`).
- Auto-compaction → `std/agent/autocompact` (`agent_autocompact_if_needed`).
- Goal recitation / scratchpad → `std/agent/scratchpad`.
- Preset packs bundle several of the above → `std/agent/presets` (`agent_preset`);
  the pack keys are `budget`, `completion_gate`, `fallback_chain`, `lane_policy`,
  `model_ladder`, `overlay_policy`, `provider`, `timeout_ms`.

## Removed in 0.10 (do not reintroduce)

- `llm_retries` / `llm_backoff_ms` are gone — compose transport retry on the
  caller seam with `with_retry` from `std/llm/handlers`, or let `agent_preset`
  bake the default bounded retry. `transcript_policy` is gone — sessions are the
  sole surface. See `docs/src/migrations/v0.10.md`.

## Trust boundary

- Harn owns orchestration.
- Harn owns transcript lifecycle.
- Harn owns replay and eval contracts.
- Harn owns delegated-worker lineage.
- Harn owns mutation-session audit metadata.
- Hosts own approval UX.
- Hosts own concrete file mutations.
- Hosts own undo and redo semantics.
- Do not silently widen shell, filesystem, network, MCP, or host-call access.
- Keep capabilities explicit in script inputs, manifests, and receipts.

## Design rules

- Keep orchestration in Harn when the logic is workflow composition.
- Rust should provide durable primitives, not one-off process plans.
- Carry session ids through workflow stages when transcript continuity matters.
- Avoid ambient global state in long-running workflows.
- Make retry, resume, and cancellation behavior visible.
- Prefer deterministic handoff artifacts over free-form summaries.
- Bound fan-out with `max_concurrent`.
- Preserve ordering when downstream replay depends on it.
- Keep tool-call schemas narrow.
- Use [[harn-providers]] when provider routing affects orchestration behavior.

## Trigger and channel work

- Route external events through declared trigger sources.
- Keep connector payload schemas explicit.
- Avoid hidden coupling between tenant, org, pipeline, and session scopes.
- Use batch filters only when they reduce real event noise.
- Make back-pressure behavior observable.
- Store enough event data for replay without leaking secrets.
- Use typed handler variants for reminder injection and lifecycle hooks.
- Keep route audits stable for CI.
- Add conformance or integration coverage for new trigger semantics.
- Update trigger quickrefs when users need a new manifest shape.

## Simpler first

- Can this be a Harn pipeline instead of a Rust command?
- Can this be a stdlib helper instead of a new VM subsystem?
- Can this reuse transcript/session builtins?
- Can this be represented as a hook event?
- Can this be a manifest option instead of imperative setup?
- Can the host provide one capability instead of several ad hoc calls?
- Can replay assert the behavior deterministically?
- Can the portal read an additive event field?
- Can the failure be reported as a diagnostic?
- Can docs explain the workflow in a small example?

## Verify

- Runtime orchestration: `cargo test -p harn-vm orchestration`.
- Agent/session behavior: `cargo test -p harn-vm agent`.
- CLI workflow commands: `cargo test -p harn-cli --test <test-name>`.
- Trigger manifests: use the relevant trigger/orchestrator checks.
- Harn examples: `cargo run --quiet --bin harn -- check <path>`.
- Conformance: `cargo run --quiet --bin harn -- test conformance --filter <name>`.
- Replay behavior: targeted replay or event-log tests.
- Portal-facing changes: `npm run portal:lint`, `portal:test`, and `portal:build`.
- Broad shared changes: `make test`.
- Docs snippets: `make check-docs-snippets`.
