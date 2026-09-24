# Transcript architecture

Harn transcripts are now versioned runtime values with three distinct layers:

- `messages`: durable conversational turns used to continue model calls.
- `events`: normalized audit history derived from messages plus lifecycle/runtime events.
- `assets`: durable descriptors for large or non-text payloads that should not be inlined into prompt history.

The intended schema is:

```json
{
  "_type": "transcript",
  "version": 2,
  "id": "tr_...",
  "state": "active",
  "summary": "optional compacted summary",
  "metadata": {},
  "messages": [
    {
      "role": "user",
      "content": [
        {"type": "image", "asset_id": "asset_1", "visibility": "public"},
        {"type": "text", "text": "Review this screenshot", "visibility": "public"}
      ]
    }
  ],
  "events": [
    {
      "kind": "message",
      "role": "user",
      "visibility": "public",
      "text": "<image:screenshot.png> Review this screenshot",
      "blocks": [...]
    }
  ],
  "assets": [
    {
      "_type": "transcript_asset",
      "id": "asset_1",
      "kind": "image",
      "mime_type": "image/png",
      "visibility": "internal",
      "storage": {"path": ".harn/assets/asset_1.png"}
    }
  ]
}
```

Rules:

- Put prompt-relevant turn content in `messages`.
- Put replay/audit/lifecycle facts in `events`.
- Put large media, file blobs, provider payload dumps, and durable attachments in `assets`.
- Store session-level system prompt material in `metadata.system_prompt` plus
  one leading internal `system_prompt` event that carries only the prompt
  fingerprint. Do not duplicate it into `messages`; provider adapters synthesize
  the correct system/developer instruction field for each request, including
  continuations that omit all new system prompt fields.
- Message blocks should reference assets by `asset_id` instead of embedding base64 when persistence matters.
- Compaction should summarize archived text while retaining asset descriptors and recent multimodal turns.
- First-class sessions enforce hard transcript retention budgets independent of
  optional auto-compaction. The runtime caps retained message count, event count,
  and any configured approximate byte budget; budget pressure either rejects the
  mutation or applies the explicit session recovery policy. Recovery writes a
  `transcript_budget` audit event, and session snapshots expose
  `metadata.transcript_budget.last_action` so hosts can explain why context was
  trimmed or compacted.

The provider's transport role does not determine product visibility. A context
directive can travel as a user-role message while its canonical event and text
blocks remain internal. The runtime uses the directive's typed identity marker
to make that distinction; matching a text prefix would also hide genuine user
messages.

Public narration accompanying a native tool call stays a message in its original
position. Separate tool lifecycle events carry the call and result. A result
updates the tool node without replacing the narration, including when one
assistant message introduces several tool calls.

These visibility and message-kind decisions happen when recording the event;
they do not rewrite older stored events. Tool input projection prefers normalized
arguments and falls back to recorded raw arguments when needed.

Persistence split:

- Hosts should persist asset files and any product-level chat/session metadata
  needed to reopen a conversation in the app shell.
- Harn run records, worker snapshots, and transcript values should persist the
  structured transcript object, including asset descriptors and message/event
  links.
- Hosts should avoid inventing a parallel hidden memory model. If a chat needs
  continuity, reuse or restore the Harn transcript and run record state.
