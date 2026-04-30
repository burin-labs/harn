# Common tasks

Use this page when you know what you want to build and need the shortest path
through the docs. Each task points to the first page to read, then the pages
that usually matter once the basic shape is working.

## Agent and automation goals

| Goal | Start here | Then read |
|---|---|---|
| Build a Slack code-review bot | [Tutorial: code review agent](./tutorial-code-review-agent.md) | [Slack Events connector](./connectors/slack-events.md), [Connector catalog](./connectors/catalog.md), [Trigger manifests](./triggers/manifest.md) |
| Run a release-audit cron | [Trigger manifests](./triggers/manifest.md) | [Cron connector](./connectors/cron.md), [Flow predicate language](./flow-predicates.md), [Maintainer release workflow](./maintainer-release.md) |
| Create an evaluated agent pipeline | [Tutorial: eval pipeline](./tutorial-eval-pipeline.md) | [Debugging agent runs](./debugging.md), [Harn portal](./portal.md), [Testing](./testing.md) |
| Add human approval before risky actions | [Human in the loop](./hitl.md) | [Trust graph](./trust-graph.md), [Host boundary](./host-boundary.md), [Workflow runtime](./workflow-runtime.md) |
| Route work across multiple agents | [Workflow runtime](./workflow-runtime.md) | [Worker dispatch](./orchestrator/worker-dispatch.md), [Agent state](./agent-state.md), [Sessions](./sessions.md) |
| Reuse a proven prompt or persona | [Prompt library stdlib](./stdlib/prompt-library.md) | [Personas](./personas.md), [Skills](./skills.md), [Skill provenance](./skill-provenance.md) |

## Integration goals

| Goal | Start here | Then read |
|---|---|---|
| Self-host an MCP server with custom tools | [Tutorial: MCP server](./tutorial-mcp-server.md) | [MCP, ACP, and A2A integration](./mcp-and-acp.md), [Outbound workflow server](./harn-serve.md), [Protocol support matrix](./protocol-support.md) |
| Connect a Cursor automation | [Orchestrator MCP Server](./mcp-server.md) | [Protocol support matrix](./protocol-support.md), [MCP, ACP, and A2A integration](./mcp-and-acp.md), [Host boundary](./host-boundary.md) |
| Connect Harn to an external MCP tool server | [Remote MCP quick start](./getting-started.md#remote-mcp-quick-start) | [MCP, ACP, and A2A integration](./mcp-and-acp.md), [Builtin functions](./builtins.md#mcp-model-context-protocol), [Configuring LLM providers](./providers.md) |
| Handle a GitHub webhook workflow | [Connector catalog](./connectors/catalog.md) | [GitHub App connector](./connectors/github.md), [Trigger manifests](./triggers/manifest.md), [Orchestrator](./orchestrator.md) |
| Author a reusable connector package | [Connector authoring](./connectors/authoring.md) | [Connector testkit](./connectors/testkit.md), [Package authoring](./package-authoring.md), [Rust connectors -> Harn packages](./migrations/rust-connectors-to-harn-packages.md) |
| Expose a workflow over HTTP or stdio | [Outbound workflow server](./harn-serve.md) | [MCP, ACP, and A2A integration](./mcp-and-acp.md), [Bridge protocol](./bridge-protocol.md), [Agents Protocol v1](./spec/agents-protocol/v1.md) |

## Operations goals

| Goal | Start here | Then read |
|---|---|---|
| Set up an agent operations console | [Harn portal](./portal.md) | [Orchestrator observability](./orchestrator/observability.md), [Trigger observability in the action graph](./observability/triggers-in-action-graph.md), [Debugging agent runs](./debugging.md) |
| Deploy a long-running orchestrator | [Orchestrator](./orchestrator.md) | [Deploy to Fly.io](./deploy/fly.md), [Deploy to Render](./deploy/render.md), [Deploy to Railway](./deploy/railway.md) |
| Manage secrets and OAuth for production | [Orchestrator secrets](./orchestrator/secrets.md) | [Connector OAuth](./orchestrator/oauth.md), [Configuring LLM providers](./providers.md), [Host boundary](./host-boundary.md) |
| Recover or replay failed trigger work | [Orchestrator DLQ management](./orchestrator/dlq.md) | [Trigger dispatcher](./triggers/dispatcher.md), [Trigger registry](./triggers/registry.md), [Debugging agent runs](./debugging.md) |
| Control throughput and backpressure | [Orchestrator backpressure](./orchestrator/backpressure.md) | [Trigger budgets](./triggers/budgets.md), [Worker dispatch](./orchestrator/worker-dispatch.md), [Concurrency](./concurrency.md) |
| Audit runtime behavior after a change | [Flow predicate language](./flow-predicates.md) | [Transcript architecture](./transcript-architecture.md), [Trust graph](./trust-graph.md), [CLI reference](./cli-reference.md#harn-flow-replay-audit) |

## If you are starting from scratch

1. Install Harn and run your first program with [Getting started](./getting-started.md).
2. Learn the core syntax with [Language basics](./language-basics.md).
3. Use [LLM calls and agent loops](./llm-and-agents.md) for model-backed work.
4. Pick one task above and follow only that path until the first version works.
5. Come back to [Best practices](./best-practices.md) before making the workflow
   write to external systems or run unattended.
