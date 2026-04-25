# Notion connector

> **Deprecated.** The Rust-side `NotionConnector` is on the sunset path tracked
> by [#446](https://github.com/burin-labs/harn/issues/446). The recommended
> implementation is the pure-Harn
> [`harn-notion-connector`](https://github.com/burin-labs/harn-notion-connector)
> package. This page documents the existing Rust connector for users who have
> not yet migrated; new deployments should configure the Harn package via
> `connector = { harn = "..." }` on the `[[providers]]` table. See the
> [Rust-to-Harn-package migration guide](../migrations/rust-connectors-to-harn-packages.md)
> for a no-downtime cutover path.

`NotionConnector` is Harn's built-in hybrid Notion integration. It combines:

- webhook ingest for the event families Notion can push directly
- polling fallback for change surfaces that do not have webhook coverage
- outbound REST helpers exposed through `std/connectors/notion`

The connector keeps both ingress paths on the same `TriggerEvent` envelope, so
handlers do not need separate webhook-vs-poll code paths.

## Inbound webhook bindings

Configure Notion as a `provider = "notion"` webhook trigger:

```toml
[[triggers]]
id = "notion-pages"
kind = "webhook"
provider = "notion"
match = { path = "/hooks/notion", events = ["page.content_updated", "comment.created"] }
handler = "handlers::on_notion"
secrets = { verification_token = "notion/verification-token" }
```

The webhook flow has two phases:

1. Notion sends a one-time POST body containing `verification_token`.
2. Harn captures that token, records it durably, and returns HTTP 200.
3. `harn doctor --no-network` reports the captured token so you can paste it
   back into the Notion integration UI and store it under
   `notion/verification-token`.
4. Later webhook deliveries are verified against `X-Notion-Signature` using
   HMAC-SHA256 over the raw request body.

Example setup loop:

```text
1. Start `harn orchestrator serve`
2. Create the Notion webhook subscription pointing at /hooks/notion
3. Trigger Notion's verification POST
4. Run `harn doctor --no-network`
5. Copy the reported token into your secret provider as notion/verification-token
6. Paste the same token into Notion's Verify subscription dialog
```

After verification, successful deliveries normalize into `TriggerEvent` with:

- `kind` from Notion's `type` field
- `dedupe_key` bucketed by entity id plus event timestamp
- `signature_status = { state: "verified" }` for signed events
- `provider_payload = NotionEventPayload`

The exported `NotionEventPayload` surface is narrowed for the most useful MVP
families:

- `subscription.verification`
- `page.content_updated`
- `page.locked`
- `comment.created`
- `data_source.schema_updated`
- polled fallback events with `payload.polled`

Notion batches some webhook events, especially `page.content_updated`, so
handlers should treat webhooks as change signals and fetch current state when
ordering or exact block diffs matter.

## Poll triggers

Notion does not publish webhook events for every interesting workspace change.
Harn supports `kind = "poll"` for the gap-filling path:

```toml
[[triggers]]
id = "notion-review-queue"
kind = "poll"
provider = "notion"
handler = "handlers::on_review_item"
secrets = { api_token = "notion/api-token" }
poll = {
  resource = "data_source",
  data_source_id = "01234567-89ab-cdef-0123-456789abcdef",
  interval_secs = 300,
  filter = { property = "Status", status = { equals = "In Review" } },
  high_water_mark = "last_edited_time",
  page_size = 100,
}
```

Supported poll config:

- `resource = "data_source"` or `resource = "database"`
- `data_source_id` or `database_id`
- `interval_secs`
- `filter`
- `sorts`
- `high_water_mark = "last_edited_time"`
- `page_size` between `1` and `100`

Polling behavior:

- Harn persists the per-trigger high-water mark in the event log.
- Each poll queries Notion for rows edited after the last persisted watermark.
- Harn stores a last-known snapshot cache per entity.
- The connector emits a normal `TriggerEvent` targeted at the owning binding.
- Synthesized poll events attach `payload.polled.before` and
  `payload.polled.after`.
- Dedupe uses the same entity-and-timestamp bucket shape as webhook events, so
  webhook and poll bindings can coexist without obvious duplicate dispatches for
  the same change bucket.

## Outbound configuration

Import from `std/connectors/notion`:

```harn
import { configure } from "std/connectors/notion"

configure({
  api_token_secret: "notion/api-token",
})
```

Required config:

- `api_token` or `api_token_secret`

Optional config:

- `api_base_url` for tests or local mocks; defaults to `https://api.notion.com/v1`
- `notion_version`; defaults to `2026-03-11`

`query_database(...)` accepts `options.resource = "database"` when you need the
legacy database query path. Otherwise it defaults to the newer data-source
query endpoint.

## Outbound helpers

Available helpers:

- `get_page(id, options = nil)`
- `update_page(id, properties, options = nil)`
- `append_blocks(page_id, blocks, options = nil)`
- `query_database(id, filter = nil, sorts = nil, options = nil)`
- `search(query, options = nil)`
- `create_comment(page_id, rich_text, options = nil)`
- `api_call(path, method, body = nil, options = nil)`

Example:

```harn
import {
  append_blocks,
  configure,
  create_comment,
  get_page,
} from "std/connectors/notion"

pipeline default() {
  configure({
    api_token_secret: "notion/api-token",
  })

  let page = get_page("01234567-89ab-cdef-0123-456789abcdef")
  append_blocks(page.id, [
    {
      object: "block",
      type: "paragraph",
      paragraph: {
        rich_text: [{type: "text", text: {content: "updated from harn"}}],
      },
    },
  ])
  create_comment(page.id, [{type: "text", text: {content: "Reviewed by Harn"}}])
}
```

## Rate limiting

The connector uses the shared `RateLimiterFactory` with a Notion API scope key
before every outbound request. It also reacts to Notion `429 rate_limited`
responses by:

- recording a rate-limit observation in `connectors.notion.rate_limit`
- honoring `Retry-After` when present
- retrying the request once after the backoff window

This matches Notion's current public guidance of an average of roughly three
requests per second per integration.
