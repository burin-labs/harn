# Coming from elsewhere

Harn's vocabulary doesn't always match what you'd read in OpenAI's, Anthropic's,
Flue's, LangGraph's, Inngest's, Mastra's, Cloudflare's, AWS Strands', BAML's, or
the ACP/A2A/MCP specs. This page is the cross-reference.

If a term in your home system collides with a Harn term, find your row in the
table for that system.

## OpenAI Agents SDK

| OpenAI term | Harn equivalent | Notes |
|---|---|---|
| `Agent` (class) | `agent_loop(harness, ...)` invocation, or a `persona` | OpenAI's `Agent` bundles instructions, tools, and output type; the closest Harn shape is a configured `agent_loop` call site. |
| `Runner.run(...)` | one `agent_loop(harness, ...)` invocation | One OpenAI "turn" wraps many model round-trips. One Harn agent loop invocation does the same. |
| **"turn"** | Harn **prompt turn** / `agent_loop` | OpenAI's "turn" is the outer cycle (one user request → final answer), not Harn's per-iteration counter. |
| "model roundtrip" (unnamed) | Harn **`iteration`** | The inner unit. |
| `max_turns` | `max_iterations` | Both bound a budget, but the nouns are off-by-one — OpenAI counts outer SDK invocations, Harn counts inner LLM calls. |
| `Session` (`SQLiteSession("id")`) | `session_id` + `harness.agent.open(id)` | Direct match. |
| `handoff` | persona handoff or `spawn_agent` | Direct match in shape. |
| `input_guardrails` / `output_guardrails` | `agent_input_guardrail` + tool middleware + completion gates | Harn exposes input guardrails as a named pre-loop bookend; output checks use completion gates, judges, validators, and tool middleware. |

## Anthropic Claude Agent SDK

| Anthropic term | Harn equivalent | Notes |
|---|---|---|
| **"agent loop"** | `agent_loop` | Direct vocabulary match. |
| `query()` / `ClaudeSDKClient` | `agent_loop(harness, ...)` | Pass `history` or reuse `session_id` for stateful prompt turns. |
| `AssistantMessage`, `TextBlock` (typed stream) | transcript events | Anthropic streams typed messages; Harn streams typed transcript events. |
| Session resumption | `harness.agent.open(id)` + transcript continuity | Direct match. |
| **"hook"** | `register_tool_hook`, `register_session_hook`, `register_reminder_provider` | Harn's hook registry is the richer version. |

Anthropic and Harn align closely on agent-loop vocabulary; this is the easiest
mapping in the table.

## LangGraph

| LangGraph term | Harn equivalent | Notes |
|---|---|---|
| `Node` | `stage` (workflow) | Both encode a unit of computation. |
| `Edge` | workflow transition | Same shape; Harn doesn't expose it as a separate noun. |
| **`State` (typed dict)** | Workflow artifacts + optional state-channel proposal | LangGraph's strict typed-dict-with-reducers is not Harn's default state model. The v0 design is tracked in [Workflow state channels](../spec/workflow-channels/v0.md). |
| **`Channel`** | proposed workflow state channel | LangGraph channels are typed slots with merge reducers. Harn's `agent_channels` are something else entirely (pub/sub for agent-to-agent communication). |
| `Thread` (`thread_id`) | `session_id` | Direct match. |
| **`super-step`** | `iteration` | LangGraph's super-step is one parallel barrier; semantically Harn's per-iteration. |
| `checkpoint` / `checkpointer` | session bundle, snapshot | Direct match. |
| `interrupt` / `Command(resume=...)` | `agent_await_resumption` | Direct match. |

LangGraph's biggest *Harn-doesn't-have-this-by-default* is **typed-state
channels with reducers**. Harn's v0 design keeps artifacts and transcripts as
the common path, then adds explicit workflow state channels for structured
fan-out/reduce cases.

## Flue

Flue and Harn both put agents inside a harness and separate continuing agents
from finite workflows. Flue is a TypeScript framework. Harn is a language and
runtime with host adapters.

| Flue term | Harn equivalent | Notes |
|---|---|---|
| `defineAgent()` | configured `agent_loop` or persona | Both bind a model, instructions, tools, skills, and an execution environment. |
| Agent instance | session | Both preserve one continuing conversation identity. |
| `defineWorkflow()` | workflow or pipeline | Both describe finite work; Harn uses a typed stage graph when the graph must be inspectable. |
| Durable event stream | transcript plus EventLog | Both retain replayable runtime events. Harn also uses the EventLog for deterministic effect replay. |
| `@flue/react` hooks | Harn Apps host plus MCP Apps UI resource | Flue projects durable state into React hooks. Harn serves host-neutral app resources and lets each host own native presentation. |
| Virtual, local, or remote sandbox | Harn sandbox and host capabilities | Both keep conversation persistence separate from workspace lifetime and access policy. |
| Target | host or deployment adapter | Flue targets Node.js and Cloudflare. A Harn program runs through CLI, IDE, protocol, self-hosted, or cloud adapters. |

Start with Flue's [agent guide](https://flueframework.com/docs/guide/building-agents/),
[workflow guide](https://flueframework.com/docs/guide/workflows/),
[event reference](https://flueframework.com/docs/api/events-reference/), and
[React guide](https://flueframework.com/docs/guide/react/) when comparing a
specific developer path.

## Inngest

| Inngest term | Harn equivalent | Notes |
|---|---|---|
| `Function` | pipeline | Both are the durable unit. |
| `Run` | session, run_id | Direct match. |
| **`step.run(key, ...)`** | `step.run(key, input?, handler, options?)` | Harn memoizes completed step results in the EventLog and replays matching steps without re-invoking the handler. |
| `step.sleep` / `step.waitForEvent` | `agent_await_resumption` + `resume_when` | Conceptually equivalent. |
| `Event` | trigger event, agent event | Direct match. |
| Step replay | `step.run` + session resume + worker snapshot | Harn supports both replay-from-top memoized steps and checkpoint/snapshot resume. |

If you arrive from Inngest expecting `step.run`-style memoized replay, start
with [Durable step stdlib](../stdlib/step.md). Durable timers and event waits
remain separate primitives: use `agent_await_resumption` and `resume_when` for
long waits.

## Mastra

| Mastra term | Harn equivalent | Notes |
|---|---|---|
| `Agent` | `agent_loop` invocation | Direct match. |
| `Workflow` | workflow | Direct match. |
| **`Thread` (per-conversation)** | `session_id` | Mastra's `thread` is Harn's `session`. |
| **`Resource` (per-user/entity)** | partial — `tenant_id` covers some of it | Mastra splits per-user vs per-conversation; Harn collapses to session + tenant. |
| `working memory` / `semantic recall` | memory builtins | Conceptually similar, less typed. |

## Cloudflare Agents SDK

Cloudflare's model is the most different one on this page: an agent is a Durable
Object, so identity, state, and compute are the same thing.

| Cloudflare term | Harn equivalent | Notes |
|---|---|---|
| `Agent` (a Durable Object class) | session plus its transcript | Cloudflare fuses the agent's identity, its storage, and the compute that serves it into one addressable object. Harn keeps the session as data and lets any host run it. |
| `this.setState()` / `this.sql` | session state, transcript, artifacts | Each Durable Object carries its own embedded SQLite. Harn's equivalents are host-provided, so the same program can run against a local file or a database. |
| `@callable` method | tool, exported function | Both expose a typed entry point to a caller. |
| `this.schedule(...)` | `agent_await_resumption` + `resume_when` | Both let work sleep and wake later without a process staying alive. |
| WebSocket hibernation | session suspend and resume | Same intent: stop paying for an idle conversation without losing it. |

Cloudflare's durability comes from where the code runs. Harn's comes from the
EventLog, so you get replay on a laptop and in CI, not only in production. The
tradeoff is real in the other direction too: Cloudflare hands you global
addressing and hibernation for free, and Harn asks the host to provide them.

## AWS Strands Agents

| Strands term | Harn equivalent | Notes |
|---|---|---|
| **"agent loop"** | `agent_loop` | Direct vocabulary match, and the same inner unit. |
| `Agent(model=..., tools=...)` | configured `agent_loop` call site | Direct match in shape. |
| `@tool` decorator | tool registration | Strands infers the schema from Python type hints; Harn takes it from the declared shape. |
| `Swarm` | `spawn_agent` plus agent channels | Both fan work out to several agents that can hand off. |
| `Graph` | workflow | Both are a deterministic node graph over agent steps. |
| `Agents as Tools` | `spawn_agent` from inside a tool | Direct match. |
| Session persistence | session, snapshot | Direct match. |

Strands is explicitly model-driven: the loop decides what to do next, and the
framework's argument is that you should let it. Harn's argument is the inverse
one, that the program should decide when a step is worth a model call. Both are
reasonable, and they optimize for different failure modes. Strands recovers from
situations you did not anticipate; Harn keeps cost and behavior predictable in
the ones you did.

## BAML

BAML is the closest thing to a peer language rather than a framework, so it is
worth being precise. Both projects are pre-1.0, both are implemented in Rust,
and both argue that agent orchestration deserves language-level support rather
than another library.

| BAML term | Harn equivalent | Notes |
|---|---|---|
| `function` with a typed return | `harness.llm.call` into a declared shape | Both make "the model returns this type" a language-level promise instead of a parsing chore. |
| Schema-aligned parsing | shape coercion and diagnostics | Both repair almost-valid model output rather than failing on a stray comma. BAML's is exposed as a reusable stdlib call and published against a function-calling benchmark. |
| `spawn` / `await`, `Future<T, E>` | `spawn_agent`, structured concurrency | Both give you real concurrency. The semantics differ; see below. |
| `test` and `testset` blocks | `harn test`, replay, evals | Both treat testing a prompt as a first-class activity. |
| Client registry, retry policies | provider config, retry and fallback policy | Direct match, including retry, fallback, and round-robin wrappers. |
| Generated SDKs for Python, TypeScript, Go, Java, C#, Rust, and more | `harn serve`, [embedding in Rust](../embedding-rust.md) | The clearest difference in distribution model. BAML's primary path is generating a typed client so an existing codebase calls into it. Harn's is running the program, or embedding the runtime. |

The center of gravity differs. BAML invests in the boundary of a single model
call and in making that boundary hard for a model to get wrong, which is why it
reads well when an agent writes the code. Harn invests in the program around the
calls, which is why transcripts, replay, capability policy, and protocol
adapters are in the language.

Two differences are worth stating precisely rather than as a scoreboard.

**Concurrency.** BAML has green threads with `spawn` and `await`, and it
deliberately rejected structured concurrency: a future outlives the scope that
created it, and there is no automatic cancellation on scope exit. Harn's
concurrency is scoped. Neither is strictly better. BAML's model has no syntactic
cost for functions that ignore cancellation; Harn's makes lifetimes obvious and
leaks harder. Pick the one whose default failure mode you prefer.

**Durability.** This is a real scope difference rather than a maturity gap. BAML
has no durable execution, checkpointing, run replay, or human-in-the-loop, and
its journal is in-memory and per-run. Harn's replay, session bundles, and
approval flows are the parts BAML has not entered. On protocols, BAML has an MCP
client and deliberately keeps protocol knowledge out of its core; Harn speaks
MCP, ACP, and A2A directly.

If your problem is "this one call must return a reliable object" and you want to
keep your existing codebase, BAML solves that directly and its generated clients
are the shortest path. If your problem is "the orchestration between the calls
became the hard part, and I need to replay what happened," that is what Harn is
for.

## ACP — Agent Client Protocol

ACP is the most important map for anyone using Harn's `serve` adapter, because
we speak ACP natively.

| ACP term | Harn equivalent | Notes |
|---|---|---|
| `session/new`, `session/load`, `session/resume` | `agent_session_open`, session fork, snapshot resume | Direct match. |
| `session/prompt` | one user message → `agent_loop` invocation | Direct match. |
| **`prompt_turn`** | one **`agent_loop` invocation** | The outer user-message → final-response cycle, terminated by a typed `terminal` outcome and lossless `stop_reason`. One invocation contains many iterations. |
| `stop_reason` | `stop_reason` | Same names. |
| `available_commands` | skills, tool registry | Partial match; ACP advertises slash-style commands. |
| `Plan` (agent plan updates) | `task_ledger`, progress tool | Direct match. |
| `tool_call` | tool call | Same names. |
| `session/cancel` | `close_agent`, cancellation token | Direct match. |
| `session/request_permission` | `approval_policy`, permissions | Direct match. |

**Read this carefully if you're writing ACP integrations:** ACP's `prompt_turn`
is the *outer* concept (one user request → final response with stop reason).
Harn's loop counts *iterations*, which are model round-trips inside the prompt
turn — the transcript events fire as `iteration_start` / `iteration_end`, and
the steering seams use the same names. One `agent_loop(...)` invocation maps
to one ACP `prompt_turn` and contains many `iteration_*` events.

## A2A — Agent2Agent Protocol

| A2A term | Harn equivalent | Notes |
|---|---|---|
| `Task` | worker, agent_loop invocation | A2A's `Task` is one unit of work with lifecycle state and history. |
| `Message` | transcript event, message | Direct match. |
| `TaskState` (submitted, working, input-required, completed, ...) | `final_status`, suspended states | A2A's state machine is more explicit; Harn maps closely. |
| `Part` (TextPart, FilePart, DataPart) | block | Direct match. |
| `Artifact` | artifact | Same name. |

## MCP — Model Context Protocol

MCP deliberately avoids conversation-shape vocabulary. It defines `Tool`,
`Resource`, `Prompt`, `Sampling`, `Elicitation` — primitives on which
conversations run, not the conversations themselves.

| MCP term | Harn equivalent | Notes |
|---|---|---|
| `sampling/createMessage` | `harness.llm.call` | One model call. |
| `Tool` | tool | Same shape. |
| `Resource` | hostlib resource, transcript asset | Same shape. |
| `Prompt` | prompt template, prompt library | Same shape. |
| `Elicitation` | HITL `hitl.ask` | Server-initiated pause-and-ask pattern. |

MCP has no `turn` / `session` / `agent_loop` — those are above MCP's layer. Use
MCP as your tool surface, not as your orchestration model.

## AG-UI

AG-UI is event-based UI streaming, not loop topology. Its vocabulary (`Lifecycle
Events`, `Text Message Events`, `Tool Call Events`, `State Management Events`)
maps onto Harn's transcript event categories with different names but the same
shapes. Harn's `serve` adapter emits AG-UI-compatible events.

## Reference: SOTA links

- [OpenAI Agents SDK — Running
  agents](https://openai.github.io/openai-agents-python/running_agents/)
- [Anthropic Claude Agent SDK
  overview](https://docs.claude.com/en/agent-sdk/overview)
- [LangGraph Graph API
  overview](https://docs.langchain.com/oss/python/langgraph/graph-api)
- [Flue documentation](https://flueframework.com/docs/)
- [Inngest — step.run
  reference](https://www.inngest.com/docs/reference/functions/step-run)
- [Mastra — Memory threads and
  resources](https://mastra.ai/docs/memory/threads-and-resources)
- [Cloudflare Agents SDK](https://developers.cloudflare.com/agents/)
- [AWS Strands Agents — Agent
  loop](https://strandsagents.com/docs/user-guide/concepts/agents/agent-loop/)
- [BAML documentation](https://docs.boundaryml.com/home)
- [Agent Client
  Protocol](https://agentclientprotocol.com/get-started/introduction)
- [A2A Protocol Specification](https://a2a-protocol.org/latest/specification/)
- [Model Context
  Protocol](https://modelcontextprotocol.io/docs/getting-started/intro)
