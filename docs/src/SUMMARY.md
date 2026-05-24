# Summary

[Introduction](./introduction.md)

# Concepts

- [Start here](./concepts/index.md)
- [Mental model](./concepts/mental-model.md)
- [Glossary](./concepts/glossary.md)
- [Choosing an agent abstraction](./concepts/abstraction-ladder.md)
- [Steering seams](./concepts/steering-seams.md)
- [Coming from elsewhere](./concepts/sota-comparison.md)
- [Why Harn?](./why-harn.md)
- [Feature matrix](./feature-matrix.md)

# Tutorials

- [Getting started](./getting-started.md)
- [Workflow authoring quickstart](./workflow-authoring-quickstart.md)
- [Tutorial: code review agent](./tutorial-code-review-agent.md)
- [Tutorial: MCP server](./tutorial-mcp-server.md)
- [Tutorial: eval pipeline](./tutorial-eval-pipeline.md)
- [Tutorial: durable daemon agent](./tutorial-daemon-agent.md)

# How-to guides

- [Common tasks](./common-tasks.md)
- [Cookbook](./cookbook.md)
- [Scripting cheatsheet](./scripting-cheatsheet.md)
- [LLM quick reference](./docs/llm/harn-quickref.md)
- [Best practices](./best-practices.md)
- [Pipeline lifecycle cookbook](./cookbooks/lifecycle.md)
- [Tool hooks cookbook](./cookbooks/tool-hooks.md)
- [Channel cookbook](./cookbooks/channels.md)
- [Pool cookbook](./cookbooks/pools.md)
- [OAuth client + provider cookbook](./oauth.md)
- [Debugging agent runs](./debugging.md)

# Reference

## Language

- [Language basics](./language-basics.md)
- [Error handling](./error-handling.md)
- [Diagnostic codes catalog](./diagnostics.md)
- [Reading shape diagnostics](./reading-shape-diagnostics.md)
- [Modules and imports](./modules.md)
- [Concurrency](./concurrency.md)
- [Streams](./streams.md)
- [Runtime context](./runtime-context.md)
- [Language specification](./language-spec.md)

## Agent runtime

- [LLM and agents](./llm-and-agents.md)
  - [LLM calls](./llm/llm_call.md)
  - [LLM handler helpers](./llm/handlers.md)
  - [LLM reranking](./llm/rerank.md)
  - [Agent loops](./llm/agent_loop.md)
  - [Prompt optimization](./llm/optimize.md)
  - [Composable callers and middleware](./stdlib/llm-handlers.md)
  - [Composable tool middleware](./stdlib/tool-middleware.md)
  - [Tools, Tool Vault, and MCP](./llm/tools.md)
  - [LLM ensemble helpers](./llm/ensemble.md)
  - [Streaming and transcripts](./llm/streaming.md)
  - [Transcript projection](./llm/transcript-projection.md)
  - [LLM providers](./llm/providers.md)
  - [Provider capability matrix](./provider-matrix.md)
  - [Provider support recommendations](./provider-support.md)
  - [Provider catalog refresh workflow](./llm/provider-catalog-refresh.md)
  - [Coding agent provider benchmark](./llm/coding-agent-benchmark.md)
- [Layered runtime configuration](./configuration.md)
- [Long-running tools](./long-running-tools.md)
- [Tool surface validation](./tool-surface-validation.md)
- [Durable step stdlib](./stdlib/step.md)
- [Cache stdlib](./stdlib/cache.md)
- [Calendar stdlib](./stdlib/calendar.md)
- [Daemon stdlib](./stdlib/daemon.md)
- [Current session builtin](./stdlib/agent_session_current_id.md)
- [Runtime introspection tools](./stdlib/runtime-introspection.md)
- [Monitor stdlib](./stdlib/monitors.md)
- [Pool stdlib](./stdlib/lifecycle-pool.md)
- [Pipeline lifecycle](./pipeline-lifecycle.md)
- [Pipeline lifecycle presets](./stdlib/lifecycle.md)
- [Observability stdlib](./stdlib/observability.md)
- [Timing stdlib](./stdlib/timing.md)
- [GraphQL stdlib](./stdlib/graphql.md)
- [OAuth storage stdlib](./stdlib/oauth-storage.md)
- [Prompt library stdlib](./stdlib/prompt-library.md)
- [Human in the loop](./hitl.md)
- [Trust graph](./trust-graph.md)
  - [Autonomy tiers](./autonomy.md)
- [Audit receipts](./audit-receipts.md)
- [Redaction policy](./redaction.md)
- [Hooks (tool, persona, session lifecycle)](./extensibility/hooks.md)
- [Preset tool hooks](./tool-hooks.md)
  - [Contributing preset hooks](./contributing/preset-hooks.md)
- [Context maintenance hooks](./context-maintenance-hooks.md)
- [Skills](./skills.md)
- [Personas](./personas.md)
  - [Persona Prelude](./personas/prelude.md)
  - [Per-stage tool scoping](./personas/stages.md)
  - [Handoff policy overrides](./personas/handoff.md)
  - [Profile bulletins](./personas/profile-bulletins.md)
  - [Merge Captain](./personas/merge-captain.md)
- [Skill provenance](./skill-provenance.md)
- [Sessions](./sessions.md)
- [Session bundles](./session-bundles.md)
- [Agent state](./agent-state.md)
- [Agent lifecycle: suspend, resume, self-park](./agent-lifecycle.md)
- [Memory](./memory.md)
- [Transcript architecture](./transcript-architecture.md)
- [System reminders](./system-reminders.md)
- [Workflow runtime](./workflow-runtime.md)
- [Portable workflow bundles](./workflow-bundles.md)
- [Local workflow supervisor](./workflow-supervisor.md)
- [Governed Code Mode](./code-mode.md)
- [Team learning and context packs](./team-learning.md)
- [Workflow crystallization](./workflow-crystallization.md)
- [Flow predicate language](./flow-predicates.md)

## Protocols

- [Protocol support matrix](./protocol-support.md)
- [MCP, ACP, and A2A integration](./mcp-and-acp.md)
- [Outbound workflow server](./harn-serve.md)
- [Bridge protocol](./bridge-protocol.md)
- [Generated protocol artifacts](./protocol-artifacts.md)
- [Host tools over the bridge](./bridge/host-tools.md)
- [ACP over WebSocket](./acp/websocket.md)
- [Harn ACP/MCP extensions v1](./spec/harn-extensions/v1.md)
- [MCP Apps UI resources](./interop/ui-resource.md)
- [Agents Protocol v1](./spec/agents-protocol/v1.md)
- [Agents Protocol Receipt Format](./spec/agents-protocol/receipt-format-v1.md)
- [Agents Protocol Replay Contract](./spec/agents-protocol/replay-v1.md)

## Orchestration

- [Triggers](./triggers.md)
- [Trigger stdlib](./stdlib/triggers.md)
- [Trigger manifests](./triggers/manifest.md)
- [Trigger budgets](./triggers/budgets.md)
- [Trigger event schema](./triggers/event-schema.md)
- [Trigger dispatcher](./triggers/dispatcher.md)
- [Trigger registry](./triggers/registry.md)
- [Webhook intake substrate](./triggers/webhook-intake.md)
- [Agent channels](./agent-channels.md)
- [Agent pools](./agent-pools.md)
- [Orchestrator](./orchestrator.md)
- [Hot reload](./orchestrator/hot-reload.md)
- [Orchestrator DLQ management](./orchestrator/dlq.md)
- [Dashboard job envelopes](./orchestrator/dashboard-jobs.md)
- [Orchestrator backpressure](./orchestrator/backpressure.md)
- [Worker dispatch](./orchestrator/worker-dispatch.md)
- [Local and A2A dispatch](./orchestrator/local-a2a-dispatch.md)
- [Orchestrator secrets](./orchestrator/secrets.md)
- [Multi-tenant orchestrator](./orchestrator/multi-tenant.md)
- [Connector OAuth](./orchestrator/oauth.md)
- [Orchestrator MCP server](./mcp-server.md)

## Packages and connectors

- [Package authoring](./package-authoring.md)
- [Connector authoring](./connectors/authoring.md)
- [Connector architecture status](./connectors/architecture.md)
- [Connector parity matrix](./connectors/parity-matrix.md)
- [Connector catalog](./connectors/catalog.md)
- [Connector testkit](./connectors/testkit.md)
- [Triage inbox envelopes](./connectors/triage-inbox.md)
- [Cron connector](./connectors/cron.md)
- [GitHub App connector](./connectors/github.md)
- [Linear connector](./connectors/linear.md)
- [Notion connector](./connectors/notion.md)
- [Slack Events connector](./connectors/slack-events.md)
- [Generic webhook connector](./connectors/webhook.md)
- [A2A push connector](./connectors/a2a-push.md)

## Observability

- [Harn portal](./portal.md)
- [Unified observability API](./observability/unified-api.md)
- [Trigger observability in the action graph](./observability/triggers-in-action-graph.md)
- [Orchestrator observability](./orchestrator/observability.md)
- [Replay benchmarks](./observability/replay-benchmarks.md)
- [Tool-call spans](./observability/tool-call-spans.md)

## CLI and tooling

- [CLI reference](./cli-reference.md)
- [CLI `--json` contract](./cli-json-contract.md)
- [`std/cli/argparse`](./cli-argparse-reference.md)
- [`std/cli/render`](./cli-render-reference.md)
- [Builtin functions](./builtins.md)
- [Postgres](./postgres.md)
- [Project scanning](./project-scan.md)
- [Prompt templating](./prompt-templating.md)
- [Editor integration](./editor-integration.md)
- [Testing](./testing.md)
- [Secret store (hostlib)](./hostlib/secret_store.md)
- [Staged filesystem (hostlib)](./hostlib/staged-fs.md)
- [Per-tool-call FS snapshots (hostlib)](./hostlib/fs-snapshot.md)

# Explanation

## Architecture

- [Host boundary](./host-boundary.md)
- [Process sandboxing](./sandboxing.md)
- [OpenTrustGraph v0 spec](./spec/open-trust-graph/v0.md)
- [Agent loop runtime notes](./dev/agent-loops.md)

## Protocol contributions

- [Protocol contribution RFCs](./protocol-contributions/README.md)
  - [ACP: `session/inject_reminder`](./protocol-contributions/acp-session-inject-reminder.md)
  - [A2A: `tasks/inject_reminder`](./protocol-contributions/a2a-message-kind-reminder.md)
  - [MCP: `notifications/reminder`](./protocol-contributions/mcp-notifications-reminder.md)
  - [ACP: `session/suspend`](./protocol-contributions/acp-session-suspend.md)
  - [A2A: `TaskState.PAUSED`](./protocol-contributions/a2a-paused-state.md)

## Architecture decisions

- [ADR 0001: Pipe operator](./adr/0001-pipe-operator.md)
- [ADR 0002: Compile-time capability invariants](./adr/0002-compile-time-capability-invariants.md)

# Operations

- [Playground](./playground.md)
- [Deploy to Render](./deploy/render.md)
- [Deploy to Fly.io](./deploy/fly.md)
- [Deploy to Railway](./deploy/railway.md)
- [Maintainer release workflow](./maintainer-release.md)
- [Release assets manifest](./dev/release-assets-manifest.md)
- [Platform compatibility](./dev/platform-compatibility.md)
- [Windows test coverage](./dev/windows-test-coverage.md)
- [Deterministic test patterns](./dev/testing.md)
- [Testbench mode](./dev/testbench.md)
- [Tape format](./dev/tape-format.md)
- [Annotation tape format](./dev/annotation-tape-format.md)
- [DES runtime mode](./dev/des-mode.md)
- [VM and stdlib perf notes](./dev/vm-stdlib-perf-notes.md)
- [Bytecode cache](./perf/bytecode-cache.md)

# Migrations

- [0.6.x → 0.7.0](./migrations/v0.7.md)
- [Prompt templates: v2](./migrations/template-engine-v2.md)
- [Package-root prompt assets](./migrations/package-root-prompt-assets.md)
- [Schema-as-type](./migrations/schema-as-type.md)
- [Rust connectors → Harn packages](./migrations/rust-connectors-to-harn-packages.md)
- [harn-hostlib host contracts](./migrations/harn-hostlib-host-contracts.md)
