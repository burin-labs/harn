<!-- markdownlint-disable MD013 -->

# How Harn compares

This page is for a developer who runs agents on another system and is deciding
whether to move them to Harn. Each row asks where a primitive lives and what the
runtime guarantees by default, so you can find out quickly whether Harn is the
wrong tool for your problem.

The comparison covers platform shape, not product quality. A `No` in a column
often means the system chose a different scope, not that it fell short.

Every non-Harn claim was checked against that system's public documentation in
August 2026. These systems ship quickly. Where a linked source disagrees with a
row here, the source is right; open an issue and the row gets fixed.

## Capabilities

Pick the systems you're comparing against. Select a capability to read what its
rating covers, or hover a cell for the one-line reason behind it. The rows at
the end are ones Harn doesn't win.

{{#comparison-matrix}}

## How to read the table

| Rating | Meaning |
|---|---|
| Yes | The core platform provides an explicit contract or runtime primitive. |
| Partial | The outcome needs application code, a paid plan, or deployment setup outside the core programming model. |
| No | The system doesn't publish the capability as a platform primitive. |
| — | Not checked against that system's documentation yet. |

## What each row covers

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
steps, so a model call inside a durable step isn't sent again on resume.
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
core orchestration model isn't reserved for a hosted service.

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

### Sandboxed by default

Agents run code and spawn commands, so the interesting question isn't whether a
sandbox is available. It's whether one is on before anyone remembers to ask.

`harn run` confines a script to its own project directory before the VM starts,
and the operating system confines any subprocess the script spawns, using
Landlock on Linux, `sandbox-exec` on macOS, and AppContainer on Windows. The
default side-effect ceiling stops below `network`, so a script can touch its own
files but can't open a socket until a run grants it.

Widening it is per-path rather than all-or-nothing: `--write-root` and
`--read-only-root` add one root for Harn and its children, `--sandbox-write-root`
and `--sandbox-read-root` add one for children only. A run that widens anything
prints the root it widened.

`--no-sandbox` turns confinement off for a single run and warns when you use it.
It also rejects the four root flags, so a run can't half-escape. Environment
policy stays in force either way, so opting out of the filesystem sandbox doesn't
hand a script your secrets.

The systems in the other columns run your workflow in your own process with your
own permissions, which is the ordinary library contract and not a shortcoming.
Isolation there is the deployment's job.

See [Process sandboxing](./sandboxing.md) and [Host boundary](./host-boundary.md).

### Signed, versioned packages

A Harn package is a versioned unit with a lockfile, stable exports, and a
`harn package verify` contract. Filesystem-backed skills can go further: a
project can require an Ed25519 signature chain before
`harness.agent.load_skill(...)` promotes a skill's body into an agent session, so a model doesn't silently load prompt
instructions off disk.

Read the other columns here carefully, because `Partial` is doing real work. A
framework living inside npm or PyPI inherits a much larger packaging ecosystem
than Harn's, and those ecosystems have their own signing stories. The difference
is what gets packaged: there, the workflow is ordinary source inside a package,
while here the workflow and the instructions an agent loads are the unit that
carries a version and a signature.

See [Package authoring](./package-authoring.md) and
[Skill provenance](./skill-provenance.md).

## Where Harn is the wrong choice

The rows above are ones Harn was built to win. These are the ones it doesn't,
and they are the fastest way to find out that another system fits your problem
better.

### Callable from an existing codebase

Harn runs a program. The
[Python](https://github.com/burin-labs/harn-sdk-python) and
[TypeScript](https://github.com/burin-labs/harn-sdk-typescript) SDKs are REST
clients for the Harn Agents API, so they need a running server rather than
linking Harn into your process. In-process means embedding the runtime in Rust,
and the other way in is over MCP, ACP, or A2A. All of those are heavier
boundaries than a library call.

BAML sits at the other end of this: its primary path is generating a typed
client for Python, TypeScript, Go, Java, C#, or Rust so an existing codebase
calls into it. If your problem is one call that must return a reliable object
and you want to keep the codebase you have, that's the shorter path.

See [MCP, ACP, and A2A integration](./mcp-and-acp.md) and
[embedding in Rust](./embedding-rust.md).

### Reuse your language's libraries

A workflow written in Harn can't reach for an arbitrary package from PyPI or
npm. The stdlib and host capabilities cover the orchestration surface, and the
host boundary covers the rest, but a framework written in your language lets you
import anything you already depend on. If your workflow leans on a specific
library, count that cost before moving.

See [Host boundary](./host-boundary.md) and the
[scripting cheatsheet](./scripting-cheatsheet.md).

### Managed hosting from the vendor

Self-hosting is the first-class path. Temporal Cloud, Inngest Cloud, and
LangGraph Platform are mature managed products; Harn's managed path is early. If
you want someone else to run the control plane today, they are ahead.

See [Orchestrator](./orchestrator.md), [Deploy to Render](./deploy/render.md),
[Deploy to Fly.io](./deploy/fly.md), and [Deploy to Railway](./deploy/railway.md).

### Proven at production scale

Harn is pre-1.0, and surface-level breaking changes are possible between minor
and patch releases. Temporal has years of at-scale production operation behind
it. If you're putting revenue-critical work on an orchestrator this quarter,
that difference is the whole decision.

For a side project, an internal tool, or an experimental alpha, that same
difference costs you very little. If that's the work you have, Harn is worth a
try, and the rough edges you hit are the most useful thing you can send back:
[open an issue](https://github.com/burin-labs/harn/issues/new).

See the [changelog](https://github.com/burin-labs/harn/blob/main/CHANGELOG.md).

### Third-party integration catalog

Harn ships a small connector set on purpose, and the LangChain ecosystem around
LangGraph is the largest in this table by a wide margin. If your work is mostly
gluing together many third-party services, you will write more of that glue
yourself here.

See the [connector catalog](./connectors/catalog.md).

### Futures that outlive their scope

Harn's concurrency is scoped: a spawned task doesn't outlive the scope that
created it, and leaving a scope cancels what it started. That makes lifetimes
obvious and leaks harder, and it means you can't fire off work that keeps
running after its caller returns.

BAML rejected structured concurrency deliberately, so a future there outlives
its creating scope with no automatic cancellation. Neither default is better.
BAML's has no syntactic cost for functions that ignore cancellation; Harn's
makes lifetimes explicit. Pick the one whose failure mode you would rather
debug.

See [Concurrency](./concurrency.md) and
[Coming from elsewhere](./concepts/sota-comparison.md).

### Small install

Harn requires you to download a runtime, and you probably already have a package
manager you're happy with. So the question is whether Harn is the thing you're
deploying, or an addition to something you already deploy.

If it's the thing you're deploying, this is a one-time cost you'd pay for any
runtime. If you're adding one typed model call to an existing service, it's tens
of megabytes and a new artifact in a pipeline that already knew how to install
packages. A framework in your own language avoids both.

As of `v0.10.114`, released 2026-08-24:

| Platform | Download |
|---|---|
| macOS arm64 | 71.1 MB |
| Linux arm64 | 74.3 MB |
| Linux x86_64 | 76.6 MB |
| macOS x86_64 | 78.6 MB |
| Windows x86_64 | 232.3 MB |

The Windows number is not the runtime's real weight, and shouldn't be read as
one. Harn ships a single multi-call binary; `harn-lsp` and `harn-dap` are the
same executable reached through `argv[0]`. On Unix the archive stores them as
symlinks, so it carries one binary. Windows has no dependable unprivileged
symlink, so that archive carried three identical copies and `.zip` compressed
each in full. Divide by three and Windows lands at 77.4 MB, within a megabyte of
Linux x86_64.

That's fixed on `main`: the archive now ships one executable and the installer
creates the two aliases. `v0.10.114` predates the fix, so the figure above is
what you would download today, and the next release should put Windows beside
the other platforms.

The other 71 to 79 MB is the runtime itself, and no work is currently tracked to
reduce it.

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
- Cursor documents per-agent Firecracker microVM isolation and a separate AWS
  account for cloud agent execution:
  <https://cursor.com/docs/cloud-agent/security>.
- Temporal documents that its Python workflow sandbox isolates global state and
  restricts non-deterministic calls, and states directly that it is not
  completely isolated:
  <https://docs.temporal.io/develop/python/python-sdk-sandbox>.
