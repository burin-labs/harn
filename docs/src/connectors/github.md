# GitHub connector

GitHub provider behavior lives in the pure-Harn
[`harn-github-connector`](https://github.com/burin-labs/harn-github-connector)
package. Harn core keeps the shared trigger envelope, webhook signature
primitives, inbox/dedupe path, and provider schema metadata; the connector
package owns GitHub-specific normalization, outbound calls, fixtures, and
release cadence.

## Install

```sh
harn add github.com/burin-labs/harn-github-connector@v0.2.0
harn connector test . --provider github
```

Wire the package through the provider manifest entry:

```toml
[[providers]]
id = "github"
connector = { harn = "harn-github-connector" }

[[triggers]]
id = "github-prs"
kind = "webhook"
provider = "github"
match = { path = "/hooks/github", events = ["pull_request.opened"] }
handler = "handlers::on_github"
dedupe_key = "event.dedupe_key"
secrets = { signing_secret = "github/webhook-secret" }
```

## Runtime contract

The package verifies GitHub webhook signatures, normalizes GitHub delivery
headers into `TriggerEvent`, and exposes outbound GitHub App calls through its
connector exports. The core runtime treats those exports like any other
connector contract v1 package.

Use the generic `webhook` provider only for forge-agnostic intake. Use
`provider = "github"` with the package above when handlers need GitHub event
families such as `issues`, `pull_request`, `issue_comment`,
`pull_request_review`, `push`, `workflow_run`, `deployment_status`,
`check_run`, `check_suite`, `status`, `merge_group`, `installation`, or
`installation_repositories`.

## Verification

```sh
harn connector check .
harn connector test . --provider github
```

For local development against this repository's CLI:

```sh
cargo run --quiet --bin harn -- connector test /path/to/harn-github-connector --provider github
```
