# Slack events connector

Slack provider behavior lives in the pure-Harn
[`harn-slack-connector`](https://github.com/burin-labs/harn-slack-connector)
package. Harn core keeps the trigger envelope, inbox/dedupe path, effect-policy
enforcement, metrics, and shared signature helpers; the connector package owns
Slack Events API normalization, URL verification, outbound Web API calls, and
Socket Mode policy.

## Install

```sh
harn add github.com/burin-labs/harn-slack-connector@v0.1.0
harn connector test . --provider slack
```

Wire the package through the provider manifest entry:

```toml
[[providers]]
id = "slack"
connector = { harn = "harn-slack-connector" }

[[triggers]]
id = "slack-events"
kind = "webhook"
provider = "slack"
match = { path = "/hooks/slack", events = ["message.channels", "app_mention"] }
handler = "handlers::on_slack"
secrets = { signing_secret = "slack/signing-secret" }
```

## Runtime contract

The package verifies Slack request signatures, handles `url_verification`
responses, emits ack-first normalized events, and exposes outbound Slack Web
API calls through connector contract v1 exports.

Slack retries if the listener does not answer quickly, so connector packages
should keep inbound normalization deterministic and push slow work to trigger
handlers. The runtime enforces hot-path effect policy for connector exports.

## Verification

```sh
harn connector check .
harn connector test . --provider slack
```

For local development against this repository's CLI:

```sh
cargo run --quiet --bin harn -- connector test /path/to/harn-slack-connector --provider slack
```
