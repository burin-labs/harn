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

## Core surfaces

- `agent_loop` drives iterative LLM/tool workflows.
- `llm_call` and `llm_stream_call` perform direct model calls.
- Tool middleware should preserve structured tool names and arguments.
- `agent_spawn` and handoffs should carry explicit session context.
- Hooks should return typed decisions, not prose protocols.
- Triggers connect external events to Harn workflows.
- Connectors normalize external payloads before script code sees them.
- Daemon mode should keep restart and replay behavior clear.
- Receipts should describe work done and decisions made.
- Mutation sessions belong at the host boundary.

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
