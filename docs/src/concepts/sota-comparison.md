# Coming from elsewhere

Harn's vocabulary doesn't always match what you'd read in OpenAI's, Anthropic's,
LangGraph's, Inngest's, Mastra's, or the ACP/A2A/MCP specs. This page is the
cross-reference.

If a term in your home system collides with a Harn term, find your row in the
table for that system.

## OpenAI Agents SDK

| OpenAI term | Harn equivalent | Notes |
|---|---|---|
| `Agent` (class) | `agent_loop(...)` invocation, or a `persona` | OpenAI's `Agent` bundles instructions, tools, and output type; the closest Harn shape is a configured `agent_loop` call site. |
| `Runner.run(...)` | `agent_turn(...)` or one `agent_loop(...)` | One OpenAI "turn" wraps many model round-trips. Harn's `agent_turn` wraps the same idea. |
| **"turn"** | Harn **`prompt_turn`** / `agent_turn` | OpenAI's "turn" is the *outer* cycle (one user request → final answer). Map it to Harn's `agent_turn` wrapper, not to Harn's per-iteration counter. |
| "model roundtrip" (unnamed) | Harn **`iteration`** | The inner unit. |
| `max_turns` | `max_iterations` | Both bound a budget, but the nouns are off-by-one — OpenAI counts outer SDK invocations, Harn counts inner LLM calls. |
| `Session` (`SQLiteSession("id")`) | `session_id` + `agent_session_open(id)` | Direct match. |
| `handoff` | persona handoff or `spawn_agent` | Direct match in shape. |
| `input_guardrails` / `output_guardrails` | tool middleware + persona pre/post hooks | Harn has no single "guardrails" noun; the shape is built from hooks. |

## Anthropic Claude Agent SDK

| Anthropic term | Harn equivalent | Notes |
|---|---|---|
| **"agent loop"** | `agent_loop` | Direct vocabulary match. |
| `query()` / `ClaudeSDKClient` | `agent_loop(...)` / `agent_chat_loop(...)` | Stateless single-shot vs stateful multi-turn. |
| `AssistantMessage`, `TextBlock` (typed stream) | transcript events | Anthropic streams typed messages; Harn streams typed transcript events. |
| Session resumption | `agent_session_open(id)` + transcript continuity | Direct match. |
| **"hook"** | `register_tool_hook`, `register_session_hook`, `register_reminder_provider` | Harn's hook registry is the richer version. |

Anthropic and Harn align closely on agent-loop vocabulary; this is the easiest
mapping in the table.

## LangGraph

| LangGraph term | Harn equivalent | Notes |
|---|---|---|
| `Node` | `stage` (workflow) | Both encode a unit of computation. |
| `Edge` | workflow transition | Same shape; Harn doesn't expose it as a separate noun. |
| **`State` (typed dict)** | `transcript` + `session` (combined) | LangGraph's strict typed-dict-with-reducers does not have a direct Harn analog. |
| **`Channel`** | (no direct equivalent) | LangGraph channels are typed slots with merge reducers. Harn's `agent_channels` are something else entirely (pub/sub for agent-to-agent communication). Exploration tracked at [#2219](https://github.com/burin-labs/harn/issues/2219). |
| `Thread` (`thread_id`) | `session_id` | Direct match. |
| **`super-step`** | `iteration` | LangGraph's super-step is one parallel barrier; semantically Harn's per-iteration. |
| `checkpoint` / `checkpointer` | session bundle, snapshot | Direct match. |
| `interrupt` / `Command(resume=...)` | `agent_await_resumption` | Direct match. |

LangGraph's biggest *Harn-doesn't-have-this* is **typed-state channels with
reducers**. If you arrive from LangGraph and miss them, file your use case on
[#2219](https://github.com/burin-labs/harn/issues/2219).

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

## ACP — Agent Client Protocol

ACP is the most important map for anyone using Harn's `serve` adapter, because
we speak ACP natively.

| ACP term | Harn equivalent | Notes |
|---|---|---|
| `session/new`, `session/load`, `session/resume` | `agent_session_open`, session fork, snapshot resume | Direct match. |
| `session/prompt` | one user message → `agent_loop` invocation | Direct match. |
| **`prompt_turn`** | **`agent_turn` (the wrapper)** | The outer user-message → final-response cycle, terminated by `stop_reason`. Harn's `agent_turn` is the same concept; an `agent_turn` invocation contains many iterations. |
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
the steering seams use the same names. One `agent_turn(...)` invocation maps
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
| `sampling/createMessage` | `llm_call` | One model call. |
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
- [Inngest — step.run
  reference](https://www.inngest.com/docs/reference/functions/step-run)
- [Mastra — Memory threads and
  resources](https://mastra.ai/docs/memory/threads-and-resources)
- [Agent Client
  Protocol](https://agentclientprotocol.com/get-started/introduction)
- [A2A Protocol Specification](https://a2a-protocol.org/latest/specification/)
- [Model Context
  Protocol](https://modelcontextprotocol.io/docs/getting-started/intro)
