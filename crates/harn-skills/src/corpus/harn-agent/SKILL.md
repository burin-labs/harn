---
name: harn-agent
short: Autonomous Harn agent workflows and safe task execution.
description: Use for autonomous Harn agent work, task decomposition, capability boundaries, and host interaction patterns.
when_to_use: Use when designing autonomous agent sessions, delegated workers, approval boundaries, or host capability usage.
---

# Harn agent

Use this skill when designing autonomous Harn sessions, delegated workers, task decomposition, or approval-aware host interactions.

Pair it with [[harn-orchestration]] for workflow design and [[harn-diagnostics]] for repair/autonomy decisions.

## Start here

- Use `docs/llm/harn-quickref.md` for `agent_loop`.
- Use the same quickref for tool formats, model options, and prompt templates.
- Use [[harn-tracing]] when transcripts, replay, or receipts matter.
- Agent session runtime code is mainly under `crates/harn-vm/src/agent_sessions*`.
- Bridge behavior lives near `crates/harn-vm/src/bridge.rs`.
- ACP behavior crosses VM and CLI integration tests.
- Keep autonomous behavior inspectable.
- Keep worktree-backed execution preferred for background edits.

## Agent loop design

- Give each loop a concrete task.
- Give each loop an explicit stop condition.
- Give each loop a narrow tool surface.
- Carry a session id when transcript continuity matters.
- Persist useful transcript deltas.
- Persist recovery advice when failure can be resumed.
- Persist mutation-session metadata when edits happen.
- Keep handoff artifacts structured.
- Avoid prose-only protocols for downstream automation.
- Bound retries and tool-call loops.

## Capability boundaries

- Shell access should be explicit.
- Filesystem mutation should be explicit.
- Network access should be explicit.
- MCP access should be explicit.
- Host calls should be explicit.
- Secrets should stay in host-managed capability stores.
- Approval UX belongs to hosts.
- Concrete file mutation belongs to hosts.
- Undo and redo semantics belong to hosts.
- Harn should record what happened and why.

## Autonomy and repair

- Use `harn check` before applying broad edits.
- Use `harn lint` before merging user-facing Harn code.
- Use `harn fix --plan` to inspect available repairs.
- Use `harn fix --apply` only within the permitted safety ceiling.
- Format-only repairs are safer than semantic rewrites.
- Capability-affecting repairs should escalate.
- Cross-file repairs should usually be reviewed.
- When a diagnostic code is known, use the code, not message scraping.
- If a tool result is ambiguous, ask for a smaller next step.
- Do not continue autonomous mutation after a trust-boundary violation.

## Delegation patterns

- Split independent read-only investigation from implementation.
- Give delegated workers a clear file ownership boundary.
- Tell workers they are not alone in the codebase.
- Avoid overlapping write sets.
- Ask explorers specific codebase questions.
- Ask workers to edit files directly only inside their scope.
- Keep integration decisions in the coordinating session.
- Review worker diffs before publishing.
- Rerun the narrow tests that cover integrated changes.
- Use [[harn-testing]] to pick the verification level.

## Common gotchas

- Do not assume ambient cwd state is safe for autonomous edits.
- Do not hide generated files in source changes.
- Do not widen capabilities to make a test pass.
- Do not conflate user approval with tool availability.
- Do not let compaction lose session intent.
- Do not let retry loops mask real diagnostics.
- Do not trust stale provider state.
- Do not record private prompt content in public summaries.
- Do not drop user or other-agent work when rebasing.
- Do not commit temporary logging.

## Verify

- Agent session changes: targeted `cargo test -p harn-vm agent`.
- Bridge or ACP behavior: `cargo test -p harn-cli --test acp_server_cli` when feasible.
- Orchestration behavior: `cargo test -p harn-vm orchestration`.
- Repair/autonomy behavior: targeted `harn fix` tests.
- Transcript changes: targeted replay or record tests.
- Harn workflow examples: `cargo run --quiet --bin harn -- check <path>`.
- Conformance changes: `cargo run --quiet --bin harn -- test conformance --filter <name>`.
- Flake guard for tests: `make lint-test-patterns`.
- Broad shared behavior: `make test`.
- Release-facing changes: run the release gate requested by the repo docs.
