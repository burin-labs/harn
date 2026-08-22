# Tool format contracts

An agent transcript is bound to one tool-calling contract for its lifetime.

## Invariant

- The first `agent_loop` that runs against a named session claims the session's
  `tool_format`.
- Re-entering that same session with a different `tool_format` is a runtime
  error.
- `agent_session_reset` clears the transcript and the claimed tool format.
- `agent_session_fork` and `agent_session_fork_at` copy the claimed tool format,
  because a fork reuses prompt/history produced under that contract.
- Child and sub-agent sessions start as new transcripts. They record lineage
  events on the parent, but they do not inherit the parent's messages or tool
  format and may choose a different model/tool contract.

## Model switching

Changing models inside an existing transcript is valid only when the effective
tool format stays the same. If a requested model change would cross from `text`
to `native`, or from `native` to `text`, callers must create a new transcript or
explicitly reset the current session before running the next model call. If
history must be preserved across a cross-format model switch, summarize or
compact the prior transcript into a fresh session and then claim the new
session's target `tool_format`. This avoids replaying text-protocol
instructions, text-mode tool schemas, or native-only tool messages under the
wrong harness contract.

Native text-tool fallback is opt-in compatibility behavior. The default policy
is `reject`, so a native session no longer accepts `<tool_call>` text as a
successful tool invocation unless the caller explicitly sets
`native_tool_fallback: "allow"` or `"allow_once"`.

## Builtins

- `agent_session_tool_format(session_id)` returns the claimed tool format or
  `nil`.
- `agent_session_claim_tool_format(session_id, tool_format)` claims the contract
  or errors if the session already has a different one.

## Accepted surface (teach ≡ accept)

Each `tool_format` has one taught surface and one accepted grammar. Prompts,
few-shots, and parse guidance render from `agent_tool_call_paradigm` /
`agent_render_tool_call_exemplar`; the parser entry point is
`agent_parse_tool_calls`. Conformance suite
`conformance/tests/agents/tool_format_contract_parity.harn` proves exemplars
round-trip under the matching pin.

| Format | Taught call shape | Multiline / code bodies | Name placement |
|--------|-------------------|-------------------------|----------------|
| `json` | ```` ```tool ```` fenced `{ "name", "args" }` | JSON string values (`\n`, `\"`, `\\`) | Inside the JSON object (`name`) |
| `text` | `<tool_call>name({ ... })</tool_call>` | Heredoc `key: <<EOF` … `EOF` | Bare call ident after the open tag |
| `native` | Provider `tool_calls[]` | Provider JSON string args | `function.name` |

`adaptive` is not a live format. An explicit `tool_format=adaptive` pin fails
closed with parse guidance to choose `json`, `text`, or `native`.

Reserved-token routes may remap `<tool_call>` ↔ `[[CALL]]` on the wire
(`tool_delimiter.rs`); the parser and transcript stay on the canonical tags.

## Host-authored completion guidance

Hosts render product-specific completion wording from the stable
`agent_completion_prompt_bindings` projection. See
[Rendering host completion guidance](src/llm/agent_loop.md#rendering-host-completion-guidance)
for the binding contract and checked Harn example.

## Demux survivability (openai-compat text routes)

Some Harmony hosts (notably Fireworks gpt-oss) demux model text into OpenAI
streamed `tool_calls` even when `native_tools=false` and no tools array was
sent. Observed corrupted `function.name` values are framing tokens — Harn's
wrapper tag `tool_call` and Harmony's recipient token `to` — not real tools.

Contract:

- Framing names (`tool_call`, `to`, and the other generic wrappers in
  `is_generic_wrapper_name`) never dispatch as tool names.
- When the provider lifts a complete text call into `function.name` /
  `arguments`, recover the inner bare call (or nested `<tool_call>` body).
- When the payload carries only clean look-shaped args and no inner name,
  infer `look` rather than guessing from arbitrary arg shapes.
- Edit-shaped nameless wrappers stay fail-closed so denial / parse guidance
  can correct the model instead of mis-dispatching.

Parse near-misses on a taught format inject format-correcting
`parse_guidance` (exact diagnostic + paradigm `body_hint`), never a generic
narration nudge. Meter route fitness with `harn provider tool-calibrate` /
`preferred_tool_format` pins; the Fireworks gpt-oss A/B that prefers `text`
for heredoc body fidelity remains the catalog pin for that route class.
