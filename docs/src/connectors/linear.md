# Linear connector

Linear provider behavior lives in the pure-Harn
[`harn-linear-connector`](https://github.com/burin-labs/harn-linear-connector)
package. Harn core keeps the shared trigger envelope, inbox/dedupe path,
metrics, HMAC helpers, and provider schema metadata; the connector package owns
Linear-specific webhook verification, replay-window policy, payload
normalization, outbound GraphQL calls, fixtures, and release cadence.

## Install

```sh
harn add github.com/burin-labs/harn-linear-connector@v0.1.0
harn connector test . --provider linear
```

Wire the package through the provider manifest entry:

```toml
[[providers]]
id = "linear"
connector = { harn = "harn-linear-connector" }

[[triggers]]
id = "linear-issues"
kind = "webhook"
provider = "linear"
match = { path = "/hooks/linear", events = ["issue.update", "comment.create"] }
handler = "handlers::on_linear"
secrets = { signing_secret = "linear/webhook-secret" }
```

## Runtime contract

The package normalizes Linear webhook deliveries such as `Issue`, `Comment`,
`IssueLabel`, `Project`, `Cycle`, `Customer`, and `CustomerRequest`, including
issue update diffs where available. Outbound GraphQL support is exposed by the
package through connector contract v1 exports.

## Verification

```sh
harn connector check .
harn connector test . --provider linear
```

For local development against this repository's CLI:

```sh
cargo run --quiet --bin harn -- connector test /path/to/harn-linear-connector --provider linear
```
