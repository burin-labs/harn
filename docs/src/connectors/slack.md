# Slack Events connector

`SlackConnector` is Harn's built-in Slack Events API integration. It verifies
Slack's signed webhook requests, narrows the most useful inbound event families
into typed `SlackEventPayload` variants, and exposes a small outbound Web API
client through `std/connectors/slack`.

This connector is for the HTTP Events API path, not Socket Mode.

## Inbound webhook bindings

Configure Slack as a `provider = "slack"` webhook trigger:

```toml
[[triggers]]
id = "slack-mentions"
kind = "webhook"
provider = "slack"
match = { path = "/hooks/slack" }
handler = "handlers::on_slack"
secrets = { signing_secret = "slack/signing-secret" }
```

The connector verifies:

- `X-Slack-Request-Timestamp`
- `X-Slack-Signature`
- the raw request body, using `v0:{timestamp}:{body}` and HMAC-SHA256

Slack retries aggressively if a listener does not answer within three seconds,
so Harn treats inbound Slack deliveries as ack-first:

1. verify the signature
2. normalize into `TriggerEvent`
3. append to `trigger.inbox`
4. return HTTP 200
5. let the dispatcher run handlers asynchronously after the response

`url_verification` is handled inline after signature verification and responds
with the plaintext `challenge` value, as required by Slack.

## Typed inbound payloads

`SlackEventPayload` is narrowed into these first-class variants:

- `message.channels`
- `app_mention`
- `reaction_added`
- `team_join`
- `channel_created`

Other Slack callback shapes still normalize into `SlackEventPayload::Other`
with the full `raw` envelope preserved.

## Outbound configuration

Import from `std/connectors/slack`:

```harn
import { configure } from "std/connectors/slack"

configure({
  bot_token_secret: "slack/bot-token",
})
```

Required config:

- `bot_token` or `bot_token_secret`

Optional config:

- `api_base_url` for tests or local mocks; defaults to `https://slack.com/api`

The connector uses bearer auth for every outbound request.

## Outbound helpers

Available helpers:

- `post_message(channel, text, blocks = nil, options = nil)`
- `update_message(channel, ts, text, blocks = nil, options = nil)`
- `add_reaction(channel, ts, name, options = nil)`
- `upload_file(filename, content, options = nil)`

`upload_file(...)` uses Slack's external upload flow
(`files.getUploadURLExternal` + upload URL + `files.completeUploadExternal`)
instead of the older legacy `files.upload` path.

Example:

```harn
import {
  add_reaction,
  configure,
  post_message,
} from "std/connectors/slack"

pipeline default() {
  configure({
    bot_token_secret: "slack/bot-token",
  })

  let posted = post_message("C123ABC456", "hello from harn")
  add_reaction("C123ABC456", posted.ts, "thumbsup")
}
```

## Recommended scopes

The minimal Slack scopes depend on which events and outbound methods you use.
Typical starting points:

- `app_mentions:read` for `app_mention`
- `channels:history` for `message.channels`
- `reactions:read` for `reaction_added`
- `users:read` for `team_join`
- `channels:read` for `channel_created`
- `chat:write` for message posting/updating
- `reactions:write` for reactions
- `files:write` for uploads
