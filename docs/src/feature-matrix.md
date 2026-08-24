<!-- markdownlint-disable MD013 -->

# Feature matrix

This page compares Harn with adjacent orchestration systems for teams building
event-driven LLM systems. It focuses on platform shape, not broad product
quality: the question is where the primitive lives and what portability,
governance, and replay guarantees the team gets by default.

Every claim about a non-Harn system below was re-verified against that system's
current public documentation in August 2026. These platforms ship quickly, so
treat the linked sources as authoritative where they disagree with this page,
and please open an issue if you find a row that is unfair to your system.

## At a glance

Select a capability to read what the rating covers. The short cells keep the
comparison readable; the sections below hold the details and sources.

| Capability | Harn | Inngest / AgentKit | Temporal | LangGraph | Cursor Automations |
|---|---|---|---|---|---|
| [Own orchestration language](#orchestration-language) | Yes | No | No | No | No |
| [Runtime replay contract](#runtime-replay-contract) | Yes | Partial | Yes | Partial | No |
| [Model-aware trigger predicates](#model-aware-trigger-predicates) | Yes | Partial | No | Partial | No |
| [Open source and self-hostable](#open-source-and-self-hostable) | Yes | Partial | Yes | Partial | Partial |
| [One program across environments](#one-program-across-environments) | Yes | Partial | Partial | Partial | No |
| [Cost limits in program code](#cost-limits) | Yes | Partial | Partial | Partial | Partial |
| [Human review and trust records](#human-review-and-trust) | Yes | Partial | Partial | Partial | Partial |
| [Model and infrastructure choice](#model-and-infrastructure-choice) | Yes | Partial | Partial | Partial | Partial |

## How to read the table

| Rating | Meaning |
|---|---|
| Yes | The core platform provides an explicit contract or runtime primitive. |
| Partial | The outcome needs application code, a paid plan, or deployment setup outside the core programming model. |
| No | The system does not publish the capability as a platform primitive. |

## Why these rows matter

### Orchestration language

Harn uses a purpose-built language for agent orchestration. That keeps trigger
policy, model calls, concurrency, retries, budgets, human review, and trust
metadata in one program. SDK-based systems express the same work through host
language callbacks, queue handlers, and configuration.

See [Language basics](./language-basics.md), [Workflow runtime](./workflow-runtime.md),
and [Flow predicate language](./flow-predicates.md).

### Runtime replay contract

LLM systems need replay for debugging, evaluation, and incident review. Harn's
runtime owns the transcript and event-log boundary so replay can reason about
the same model request, tool result, trigger event, approval, and dispatch
history.

Temporal replays workflow state when workflow code follows its deterministic
constraints and side effects run in Activities. Inngest memoizes completed
steps, so a model call inside a durable step is not sent again on resume.
LangGraph replay re-executes nodes after the selected checkpoint, including
model calls, which may return different results.

See [Durable step stdlib](./stdlib/step.md),
[Transcript architecture](./transcript-architecture.md), [Testing](./testing.md),
and [Trigger event schema](./triggers/event-schema.md).

### Model-aware trigger predicates

Harn treats predicates over events as runtime objects, including model-backed
classifiers and budget policy. The same trigger machinery makes those decisions
inspectable, typed, budgeted, and replayable. Other systems can call a model
before dispatch, but the application owns that classifier and its policy.

See [Triggers](./triggers.md), [Flow predicate language](./flow-predicates.md),
and [Trigger budgets](./triggers/budgets.md).

### Open source and self-hostable

Harn's open-source boundary includes the runtime substrate: language, VM,
orchestrator, EventLog contracts, connectors, protocols, and self-hostable
deployment path. A cloud platform can add managed tenancy and operations, but the
core orchestration model is not reserved for a hosted service.

See [Orchestrator](./orchestrator.md), [Deploy to Render](./deploy/render.md),
[Deploy to Fly.io](./deploy/fly.md), and [Deploy to Railway](./deploy/railway.md).

### One program across environments

A `.harn` program should remain the unit of review whether it runs as a local
script, CI job, self-hosted orchestrator workflow, MCP server, ACP backend, or
managed cloud workflow. That portability is the practical payoff of keeping the
workflow in one language and putting host-specific details at the boundary.

See [Harn portal](./portal.md), [Outbound workflow server](./harn-serve.md),
[MCP, ACP, and A2A integration](./mcp-and-acp.md), and
[Host boundary](./host-boundary.md).

### Cost limits

Agent systems fail operationally when model calls, retries, and background
triggers become invisible. Harn exposes trigger budgets and runtime context so
teams can place limits next to the workflow. Provider billing pages still show
account-wide spend.

See [Trigger budgets](./triggers/budgets.md), [Runtime context](./runtime-context.md),
and [LLM providers](./llm/providers.md).

### Human review and trust

Human-in-the-loop (HITL) work pauses an agent for review, approval, or input.
Harn records that step with agent session lineage and trust graph data, so the
review remains part of the orchestration and audit trail.

See [Human in the loop](./hitl.md), [Trust graph](./trust-graph.md),
[Sessions](./sessions.md), and [Agent state](./agent-state.md).

### Model and infrastructure choice

Harn is model-neutral by design. Workflows can target hosted providers,
OpenAI-compatible endpoints, local model servers, Ollama, or a provider chosen
by a team. The team can keep models and the runtime inside its own
infrastructure, including networks without public internet access, when its
providers support that setup.

See [LLM providers](./llm/providers.md), [Provider capability matrix](./provider-matrix.md),
and [Orchestrator secrets](./orchestrator/secrets.md).

## Public references

- Inngest documents SDK-defined AI workflows, AgentKit, durable steps, flow
  control, and self-hosting in its public docs and repository:
  <https://www.inngest.com/ai> and <https://github.com/inngest/inngest>.
- Temporal describes open-source durable workflows, event histories, and
  deterministic workflow constraints in its docs:
  <https://docs.temporal.io/> and <https://docs.temporal.io/workflows>.
- LangGraph documents durable execution, checkpointing, interrupts, and
  human-in-the-loop patterns:
  <https://docs.langchain.com/oss/python/langgraph/overview> and
  <https://docs.langchain.com/oss/python/langgraph/interrupts>. Its time-travel
  documentation states directly that replay re-executes nodes rather than
  reading from a cache:
  <https://docs.langchain.com/oss/python/langgraph/use-time-travel>.
- Inngest AgentKit documents tool-approval human-in-the-loop:
  <https://agentkit.inngest.com/reference/react-hooks/use-chat>.
- Temporal documents its LLM SDK integrations, and announced the experimental
  Temporal Agent Harness in August 2026:
  <https://docs.temporal.io/develop/python/integrations/openai-agents> and
  <https://temporal.io/blog/temporal-agent-harness-durable-agent-infrastructure>.
- Cursor documents self-hosted agent pools and Automations:
  <https://cursor.com/docs/cloud-agent/self-hosted-guides/pool> and
  <https://cursor.com/docs/cloud-agent/automations>.
- Cursor announced Automations and self-hosted cloud agents in its public
  changelog:
  <https://cursor.com/changelog/03-05-26> and
  <https://cursor.com/changelog/03-25-26>.
