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

| Capability | Harn | Inngest / AgentKit | Temporal | LangGraph | Cursor Automations |
|---|---|---|---|---|---|
| Typed DSL? | Yes. Harn programs are a typed orchestration language. | No. Workflows are written through language SDKs and AgentKit libraries. | No. Workflows are general-purpose-language code with SDK constraints. | No. Graphs are Python or JavaScript objects and functions. | No. Automations are trigger plus instruction configurations for Cursor agents. |
| Deterministic replay? | Yes. Replay is a runtime contract around the EventLog, `step.run` memoized effects, VM effects, transcripts, and trigger delivery. | Partial. Durable steps memoize work and resume runs, and `step.ai.wrap`/`step.ai.infer` provide first-class memoized model calls; replay fidelity still depends on the application using them. | Yes for workflow state. Workflow code must remain deterministic and side effects move into Activities. | Partial. Checkpoints resume stateful graph runs; node code and model calls still need app-level discipline. | No public deterministic replay contract for arbitrary automations. |
| LLM-native trigger predicates? | Yes. Flow predicates and trigger budgets are part of the runtime surface. | Partial. AI calls can be durable steps, but trigger classification is application code. | No. Temporal can orchestrate an LLM classifier Activity, but predicates are not a workflow-language primitive. | Partial. Conditional edges can use model output, but this is graph logic rather than a trigger substrate. | No. Event and schedule triggers launch instruction-driven agents; predicates are not typed public runtime objects. |
| OSS and self-hostable? | Yes. The language, runtime, orchestrator, connectors, and docs are open and self-hostable. | Partial. SDKs are Apache-2.0, while the server/CLI use SSPL plus delayed Apache publication and can be self-hosted. | Yes. Temporal is open source and can be self-hosted, with Temporal Cloud available. | Partial. LangGraph is open source; the full managed platform/self-hosted control plane is tied to LangSmith plans. | Partial. Automations can be routed to self-hosted workers for tool execution, but the agent loop, orchestration, and model access stay in Cursor's cloud, and Cursor itself is not open source. |
| Same program, any deploy? | Yes. The same `.harn` program can run locally, in CI, in a self-hosted orchestrator, or behind a managed cloud platform. | Partial. Functions deploy to app infrastructure but depend on Inngest's event/executor contract. | Partial. Workflow code is portable across Temporal deployments, but Activities, workers, and task queues are deployment-shaped. | Partial. Graph code can move between local and platform deployments, with production behavior depending on checkpointers and LangSmith deployment choices. | No. Automations are Cursor-managed agent workflows. |
| Cost governance? | Yes. Trigger budgets, model routing, and runtime context make cost limits a first-class policy concern. | Partial. Flow control covers concurrency, throttling, rate limits, and queues, and per-call model cost and token telemetry is automatic; spend *enforcement* policy is app-level. | Partial. Temporal controls retries, schedules, workers, and task queues; LLM cost policy is app-level. | Partial. Model selection and limits are usually app or LangSmith configuration. | Partial. Usage controls are product-level rather than per-program model-routing policy. |
| Trust graph and HITL primitives? | Yes. Harn has explicit HITL stdlib, approval patterns, trust graph records, and transcript lineage. | Partial. AgentKit ships first-class tool-approval HITL (`approveToolCall`/`denyToolCall`, an `awaiting-approval` tool state, and an approval API route), and propagated sessions group a run tree by lineage; there is no separate trust graph data model. | Partial. Signals, Updates, and Activities can implement approval workflows. The experimental Temporal Agent Harness (announced August 2026, earlier than public preview) adds an LLM-specific tool-approval policy layer with custom predicates and a structured `AgentEvent` trajectory stream. | Partial. Interrupts support human review and state edits; trust graph semantics are app-level. | Partial. Cursor has review-oriented agent UX, but no public trust graph data model. |
| BYO model and sovereign deploy? | Yes. Provider configuration covers hosted APIs, OpenAI-compatible endpoints, Ollama, local servers, and self-hosted deployment. | Partial. AgentKit supports multiple model providers; sovereign deployment depends on the app, Inngest deployment, and provider setup. | Partial. Temporal can run in sovereign environments. LLM provider choice sits in Activities, though SDK plugins for the OpenAI Agents SDK, Google ADK, PydanticAI, and the Vercel AI SDK now register the model Activity for you. | Partial. LangGraph can call any model the app integrates; deployment sovereignty depends on the chosen runtime and LangSmith tier. | Partial. Cursor supports configured models and self-hosted cloud agents, but the automation surface is still Cursor-specific. |

## How to read the table

**Yes** means the capability is an explicit platform contract or first-class
runtime primitive.

**Partial** means the system can support the outcome, but the guarantee comes
from application code, paid/platform configuration, or surrounding deployment
choices rather than the core programming model.

**No** means the capability is not the system's public primitive, even if a
team could build something similar around it.

## Why these rows matter

### Typed DSL

Harn uses a purpose-built language for agent orchestration. That keeps trigger
policy, model calls, concurrency, retries, budgets, human review, and trust
metadata in the same program instead of scattering them across SDK callbacks,
queue handlers, dashboards, and prompt-only automation settings.

See [Language basics](./language-basics.md), [Workflow runtime](./workflow-runtime.md),
and [Flow predicate language](./flow-predicates.md).

### Deterministic replay

LLM systems need replay for debugging, evaluation, and incident review. Harn's
runtime owns the transcript and event-log boundary so replay can reason about
the same model request, tool result, trigger event, approval, and dispatch
history.

Temporal has strong deterministic replay for workflow state, but it requires
workflow code to obey deterministic constraints and pushes side effects into
Activities. Inngest memoizes completed steps, so a model call made inside a
durable step is not re-issued on resume, by the same mechanism as a Temporal
Activity. LangGraph is the genuine contrast, and its own documentation is
explicit about it: replay re-executes nodes rather than reading from a cache, so
model calls fire again and may return different results.

See [Durable step stdlib](./stdlib/step.md),
[Transcript architecture](./transcript-architecture.md), [Testing](./testing.md),
and [Trigger event schema](./triggers/event-schema.md).

### LLM-native trigger predicates

Harn treats predicates over events as runtime objects, including model-backed
classifiers and budget policy. The point is not that other systems cannot call
an LLM before deciding what to do. They can. The distinction is that Harn can
make that decision inspectable, typed, budgeted, and replayable through the same
trigger machinery that dispatched the work.

See [Triggers](./triggers.md), [Flow predicate language](./flow-predicates.md),
and [Trigger budgets](./triggers/budgets.md).

### Open and self-hostable

Harn's open-source boundary includes the runtime substrate: language, VM,
orchestrator, EventLog contracts, connectors, protocols, and self-hostable
deployment path. A cloud platform can add managed tenancy and operations, but the
core orchestration model is not reserved for a hosted service.

See [Orchestrator](./orchestrator.md), [Deploy to Render](./deploy/render.md),
[Deploy to Fly.io](./deploy/fly.md), and [Deploy to Railway](./deploy/railway.md).

### Same program, any deploy

A `.harn` program should remain the unit of review whether it runs as a local
script, CI job, self-hosted orchestrator workflow, MCP server, ACP backend, or
managed cloud workflow. That portability is the practical payoff of keeping the
workflow in one language and putting host-specific details at the boundary.

See [Harn portal](./portal.md), [Outbound workflow server](./harn-serve.md),
[MCP, ACP, and A2A integration](./mcp-and-acp.md), and
[Host boundary](./host-boundary.md).

### Cost governance

Agent systems fail operationally when model calls, retries, and background
triggers become invisible. Harn exposes trigger budgets and runtime context so
teams can place limits near the workflow rather than relying only on provider
billing pages or ad hoc wrapper code.

See [Trigger budgets](./triggers/budgets.md), [Runtime context](./runtime-context.md),
and [LLM providers](./llm/providers.md).

### Trust graph and HITL

Harn's supervision model is built around explicit approvals, human review,
agent session lineage, and trust graph records. HITL is not only an "approve"
button at the end of a run; it is part of the orchestration and audit trail.

See [Human in the loop](./hitl.md), [Trust graph](./trust-graph.md),
[Sessions](./sessions.md), and [Agent state](./agent-state.md).

### BYO model and sovereign deploy

Harn is model-neutral by design. Workflows can target hosted providers,
OpenAI-compatible endpoints, local model servers, Ollama, or a provider chosen
by an enterprise deployment. The same boundary supports sovereign, private, and
air-gapped deployments when the model and infrastructure allow it.

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
