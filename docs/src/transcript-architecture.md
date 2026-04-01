# Transcript Architecture

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
- Message blocks should reference assets by `asset_id` instead of embedding base64 when persistence matters.
- Compaction should summarize archived text while retaining asset descriptors and recent multimodal turns.

Persistence split:

- Hosts should persist asset files and any product-level chat/session metadata
  needed to reopen a conversation in the app shell.
- Harn run records, worker snapshots, and transcript values should persist the
  structured transcript object, including asset descriptors and message/event
  links.
- Hosts should avoid inventing a parallel hidden memory model. If a chat needs
  continuity, reuse or restore the Harn transcript and run record state.
