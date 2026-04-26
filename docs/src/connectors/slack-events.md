# Slack Events connector

> **Deprecated.** The Rust-side `SlackConnector` is a compatibility shim under
> the [#350](https://github.com/burin-labs/harn/issues/350) pure-Harn connector
> pivot. The recommended implementation is the pure-Harn
> [`harn-slack-connector`](https://github.com/burin-labs/harn-slack-connector)
> package. This page documents the existing Rust connector for users who have
> not yet migrated; new deployments should configure the Harn package via
> `connector = { harn = "..." }` on the `[[providers]]` table. See the
> [Rust-to-Harn-package migration guide](../migrations/rust-connectors-to-harn-packages.md)
> for a no-downtime cutover path.

`SlackConnector` is Harn's built-in Slack Events API integration. It verifies
Slack's signed webhook requests, narrows the event families most useful for
agent orchestration into typed `SlackEventPayload` variants, and exposes a
small outbound Web API client through `std/connectors/slack`.

This connector is for the HTTP Events API path, not Socket Mode.

## Inbound webhook bindings

Configure Slack as a `provider = "slack"` webhook trigger:

```toml
[[triggers]]
id = "slack-events"
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

Retry metadata is preserved on the normalized headers:

- `X-Slack-Retry-Num`
- `X-Slack-Retry-Reason`

Permanent client-side rejections return `x-slack-no-retry: 1` so Slack does not
keep replaying bad requests.

## Typed inbound payloads

`SlackEventPayload` is narrowed into these first-class variants:

- `Message` for `message.*` deliveries such as `message.channels`
- `AppMention`
- `ReactionAdded`
- `AppHomeOpened`
- `AssistantThreadStarted`

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
- `open_view(trigger_id, view, options = nil)`
- `user_info(user_id, options = nil)`
- `api_call(method, args = nil, options = nil)`
- `upload_file(filename, content, options = nil)`

`api_call(...)` is a POST-based escape hatch for Slack methods that are not yet
wrapped directly by the stdlib module.

`upload_file(...)` uses Slack's external upload flow
(`files.getUploadURLExternal` + upload URL + `files.completeUploadExternal`)
instead of the older legacy `files.upload` path.

Example:

```harn
import {
  add_reaction,
  configure,
  message_source,
  post_message,
  reaction_source,
  user_info,
  wait_for_message,
  wait_for_reaction,
} from "std/connectors/slack"

pipeline default() {
  configure({
    bot_token_secret: "slack/bot-token",
  })

  let user = user_info("U123ABC456")
  let posted = post_message("C123ABC456", "hello " + (user.user.name ?? "from harn"))
  add_reaction("C123ABC456", posted.ts, "thumbsup")
}
```

## Monitor helpers

`std/connectors/slack` includes push-driven monitor sources for Slack arrivals:

- `message_source(channel = nil, options = nil)` matches `message`,
  `message.*`, and `app_mention` events. Options can include `user`,
  `thread_ts`, and `text_contains`.
- `reaction_source(channel = nil, reaction = nil, options = nil)` matches
  `reaction_added` events.
- `wait_for_message(...)` and `wait_for_reaction(...)` wrap those sources with
  `std/monitors::wait_for`.

These sources use Slack Events API webhooks as the state source, so enable the
corresponding event subscriptions on the app.

## Recommended scopes

The minimal Slack scopes depend on which events and outbound methods you use.
Typical starting points:

- `app_mentions:read` for `app_mention`
- `channels:history` for `message.channels`
- `reactions:read` for `reaction_added`
- enable the App Home tab for `app_home_opened`
- `assistant:write` for `assistant_thread_started` where Slack makes that scope available
- `chat:write` for message posting and updates
- `reactions:write` for reactions
- `users:read` for `user_info`
- `users:read.email` only if you need email fields from `users.info`
- `files:write` for uploads

See `examples/slack-app-manifest.yaml` for a pasteable starting manifest.
