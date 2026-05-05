# Notion connector

Notion provider behavior lives in the pure-Harn
[`harn-notion-connector`](https://github.com/burin-labs/harn-notion-connector)
package, with outbound API typing in
[`notion-sdk-harn`](https://github.com/burin-labs/notion-sdk-harn). Harn core
keeps the shared trigger envelope, inbox/dedupe path, poll trigger primitive,
metrics, HMAC helpers, and provider schema metadata; the connector packages own
Notion-specific webhooks, polling policy, outbound API calls, fixtures, and
release cadence.

## Install

```sh
harn add github.com/burin-labs/harn-notion-connector@v0.1.0
harn connector test . --provider notion --run-poll-tick
```

Wire the package through the provider manifest entry:

```toml
[[providers]]
id = "notion"
connector = { harn = "harn-notion-connector" }

[[triggers]]
id = "notion-pages"
kind = "webhook"
provider = "notion"
match = { path = "/hooks/notion", events = ["page.content_updated", "comment.created"] }
handler = "handlers::on_notion"
secrets = { verification_token = "notion/verification-token" }
```

Polling triggers use the same provider entry:

```toml
[[triggers]]
id = "notion-database-watcher"
kind = "poll"
provider = "notion"
match = { events = ["notion.page.polled", "notion.page.content_updated"] }
poll = { interval = "5m", state_key = "notion:database:watcher", max_batch_size = 50 }
handler = "handlers::on_notion_change"
secrets = { verification_token = "notion/verification-token" }
```

## Runtime contract

The package handles subscription verification, signed webhook deliveries,
database/page polling, and outbound Notion API helpers through connector
contract v1 exports. Handlers receive ordinary `TriggerEvent` values, so
webhook and poll sources can share handler code when that is useful.

## Verification

```sh
harn connector check .
harn connector test . --provider notion --run-poll-tick
```

For local development against this repository's CLI:

```sh
cargo run --quiet --bin harn -- connector test /path/to/harn-notion-connector --provider notion --run-poll-tick
```
