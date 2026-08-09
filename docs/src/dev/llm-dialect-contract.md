# LLM dialect ownership

An LLM route has one wire dialect for its entire lifetime. Harn resolves that
dialect from capability data before building a request, then carries the same
typed contract through stream decoding, final response parsing, and HTTP error
classification. A transport cannot build an Anthropic request and later choose
an OpenAI parser from a response header.

## The contract boundary

Capability resolution supplies two facts:

| Fact | Meaning |
|---|---|
| `message_wire_format` | The request and response vocabulary: OpenAI-compatible, Anthropic, Gemini, or Ollama. |
| `live_endpoint_family` | The synchronous endpoint when one vocabulary has multiple APIs. Gemini uses this to distinguish `generateContent` from Interactions. |

Those facts resolve one internal dialect contract. The contract selects four
operations together:

1. Lower the provider-neutral request into the provider body.
2. Select and decode the stream grammar, if the route streams.
3. Parse the final provider envelope into Harn's canonical LLM result.
4. Classify a non-success HTTP response into Harn's error taxonomy.

The transport owns HTTP, authentication, deadlines, byte delivery, and raw
capture. Provider modules retain syntax-heavy JSON assembly and event scanners.
They do not independently choose which provider grammar applies.

## Why the endpoint is part of the dialect

Gemini exposes two synchronous APIs with incompatible envelopes.
`generateContent` uses `contents`, `functionCall`, and a complete JSON response.
Interactions uses typed input steps, SSE step events, and an interaction
envelope. Treating both as a single `gemini` boolean would leave the event and
response parser ambiguous, so the contract includes the resolved live endpoint
family.

Batch format remains separate. Gemini Batch accepts `generateContent`-shaped
rows even when synchronous calls use Interactions. The [provider
reference](../llm/providers.md#gemini-interactions-api) describes that public
capability split.

## Mechanics stay behind one seam

Moving every JSON loop into Harn source would increase allocation and erase
mature parser coverage without changing policy ownership. The deep seam is the
typed contract, not a package per provider: callers select one contract, while
the existing Rust modules implement the substantial mechanics hidden behind
it.

Golden fixtures pin request bytes, response semantics, stream semantics, usage,
stop reasons, and error classification for OpenAI-compatible, Anthropic,
Gemini `generateContent`, and Gemini Interactions routes. The fixtures exercise
the same builders and parsers as live dispatch. A mismatched contract is
rejected rather than decoded under a second grammar.

See the generated [provider capability matrix](../provider-matrix.md) for route
facts and [LLM providers](../llm/providers.md) for configuration reference.
