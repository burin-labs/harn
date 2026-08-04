---
name: webhook-generic-hmac
short: Customize a generic HMAC webhook trigger.
description: Generic webhook recipe with signed inbound requests.
when-to-use: Use when integrating a provider through HMAC-signed webhooks.
---
# Generic HMAC webhook

Set the signing secret in `harn.toml`, then customize `on_webhook` for the
provider payload shape.
