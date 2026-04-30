# Summary

[Introduction](./introduction.md)

# Getting Started

- [Getting started](./getting-started.md)
- [Common tasks](./common-tasks.md)
- [Scripting cheatsheet](./scripting-cheatsheet.md)
- [LLM quick reference](./docs/llm/harn-quickref.md)
- [Why Harn?](./why-harn.md)
- [Feature matrix](./feature-matrix.md)

# Language

- [Language basics](./language-basics.md)
- [Error handling](./error-handling.md)
- [Modules and imports](./modules.md)
- [Concurrency](./concurrency.md)
- [Streams](./streams.md)
- [Runtime context](./runtime-context.md)
- [Language specification](./language-spec.md)

# Agent Runtime

- [LLM and agents](./llm-and-agents.md)
  - [LLM calls](./llm/llm_call.md)
  - [Agent loops](./llm/agent_loop.md)
  - [Tools, Tool Vault, and MCP](./llm/tools.md)
  - [Streaming and transcripts](./llm/streaming.md)
  - [LLM providers](./llm/providers.md)
  - [Provider capability matrix](./provider-matrix.md)
- [Typed tools for agent loops](./typed-tools.md)
- [Long-running tools](./long-running-tools.md)
- [Tool surface validation](./tool-surface-validation.md)
- [Daemon stdlib](./stdlib/daemon.md)
- [Current session builtin](./stdlib/agent_session_current_id.md)
- [Monitor stdlib](./stdlib/monitors.md)
- [GraphQL stdlib](./stdlib/graphql.md)
- [Prompt library stdlib](./stdlib/prompt-library.md)
- [Human in the loop](./hitl.md)
- [Trust graph](./trust-graph.md)
- [Skills](./skills.md)
- [Personas](./personas.md)
  - [Merge Captain](./personas/merge-captain.md)
- [Skill provenance](./skill-provenance.md)
- [Sessions](./sessions.md)
- [Agent state](./agent-state.md)
- [Memory](./memory.md)
- [Transcript architecture](./transcript-architecture.md)
- [Workflow runtime](./workflow-runtime.md)
- [Team learning and context packs](./team-learning.md)
- [Workflow crystallization](./workflow-crystallization.md)
- [Flow predicate language](./flow-predicates.md)

# Protocols

- [Protocol support matrix](./protocol-support.md)
- [MCP, ACP, and A2A integration](./mcp-and-acp.md)
- [Outbound workflow server](./harn-serve.md)
- [Bridge protocol](./bridge-protocol.md)
- [Host tools over the bridge](./bridge/host-tools.md)
- [ACP over WebSocket](./acp/websocket.md)
- [Agents Protocol v1](./spec/agents-protocol/v1.md)
- [Agents Protocol Receipt Format](./spec/agents-protocol/receipt-format-v1.md)
- [Agents Protocol Replay Contract](./spec/agents-protocol/replay-v1.md)

# Orchestration

- [Triggers](./triggers.md)
- [Trigger stdlib](./stdlib/triggers.md)
- [Trigger manifests](./triggers/manifest.md)
- [Trigger budgets](./triggers/budgets.md)
- [Trigger event schema](./triggers/event-schema.md)
- [Trigger dispatcher](./triggers/dispatcher.md)
- [Trigger registry](./triggers/registry.md)
- [Orchestrator](./orchestrator.md)
- [Hot reload](./orchestrator/hot-reload.md)
- [Orchestrator DLQ management](./orchestrator/dlq.md)
- [Orchestrator backpressure](./orchestrator/backpressure.md)
- [Worker dispatch](./orchestrator/worker-dispatch.md)
- [Orchestrator secrets](./orchestrator/secrets.md)
- [Multi-tenant orchestrator](./orchestrator/multi-tenant.md)
- [Connector OAuth](./orchestrator/oauth.md)
- [Orchestrator MCP server](./mcp-server.md)

# Packages and Connectors

- [Package authoring](./package-authoring.md)
- [Connector authoring](./connectors/authoring.md)
- [Connector architecture status](./connectors/architecture.md)
- [Connector catalog](./connectors/catalog.md)
- [Connector testkit](./connectors/testkit.md)
- [Cron connector](./connectors/cron.md)
- [GitHub App connector](./connectors/github.md)
- [Linear connector](./connectors/linear.md)
- [Notion connector](./connectors/notion.md)
- [Slack Events connector](./connectors/slack-events.md)
- [Generic webhook connector](./connectors/webhook.md)
- [A2A push connector](./connectors/a2a-push.md)

# Observability

- [Harn portal](./portal.md)
- [Debugging agent runs](./debugging.md)
- [Trigger observability in the action graph](./observability/triggers-in-action-graph.md)
- [Orchestrator observability](./orchestrator/observability.md)

# Operations

- [Playground](./playground.md)
- [Host boundary](./host-boundary.md)
- [Deploy to Render](./deploy/render.md)
- [Deploy to Fly.io](./deploy/fly.md)
- [Deploy to Railway](./deploy/railway.md)
- [Maintainer release workflow](./maintainer-release.md)

# Tutorials and Guides

- [Cookbook](./cookbook.md)
- [Tutorial: code review agent](./tutorial-code-review-agent.md)
- [Tutorial: MCP server](./tutorial-mcp-server.md)
- [Tutorial: eval pipeline](./tutorial-eval-pipeline.md)
- [Best practices](./best-practices.md)

# Reference

- [CLI reference](./cli-reference.md)
- [Builtin functions](./builtins.md)
- [Postgres](./postgres.md)
- [Project scanning](./project-scan.md)
- [Prompt templating](./prompt-templating.md)
- [Editor integration](./editor-integration.md)
- [Testing](./testing.md)

# Migrations

- [0.6.x → 0.7.0](./migrations/v0.7.md)
- [Prompt templates: v2](./migrations/template-engine-v2.md)
- [Package-root prompt assets](./migrations/package-root-prompt-assets.md)
- [Schema-as-type](./migrations/schema-as-type.md)
- [Rust connectors → Harn packages](./migrations/rust-connectors-to-harn-packages.md)
- [harn-hostlib host contracts](./migrations/harn-hostlib-host-contracts.md)
