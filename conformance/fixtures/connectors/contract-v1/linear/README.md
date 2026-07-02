# harn-linear-connector

Pure-Harn Linear connector for the Harn orchestrator. Verifies inbound
webhook signatures, enforces Linear's 60-second replay window, normalizes
Linear event payloads to the canonical `TriggerEvent` shape, and dispatches
outbound GraphQL queries.

> **Status:** pre-alpha. This repo is a first-party connector package for
> Harn. CI installs the pinned CLI version from [`.harn-version`](./.harn-version).

This is an **inbound + outbound** connector implementing Harn Connector
Contract v1. The canonical contract docs live in the Harn repo:

- [Connector authoring](https://github.com/burin-labs/harn/blob/main/docs/src/connectors/authoring.md)
- [Connector architecture](https://github.com/burin-labs/harn/blob/main/docs/src/connectors/architecture.md)

Linear has no OpenAPI spec. Its public API is GraphQL. Outbound calls use
Harn's `std/graphql` operation/envelope helpers and shared connector HTTP
policy so query documents, errors, rate-limit metadata, and cursor pagination
are handled the same way as other GraphQL-first connector packages.

## Install

```sh
harn add github.com/burin-labs/harn-linear-connector@main
```

For local multi-repo development, a path dependency is still useful:

```toml
[dependencies]
harn-linear-connector = { path = "../harn-linear-connector" }
```

## Usage

```harn
import linear_connector from "harn-linear-connector/default"

trigger triage on linear {
  source = {
    kind: "webhook",
    signing_secret: env("LINEAR_WEBHOOK_SECRET"),
    api_key: env("LINEAR_API_KEY"),
    events: ["Issue"],
  }
  on event {
    let raw = event.provider_payload.raw
    if raw.action == "create" && raw.data.priority == 1 {
      linear_connector.call("graphql", {
        query: """
          mutation Comment($issueId: String!, $body: String!) {
            commentCreate(input: { issueId: $issueId, body: $body }) {
              success
            }
          }
        """,
        variables: { issueId: raw.data.id, body: "Auto-triaged: urgent." },
      })
    }
  }
}
```

## Configuration

Inbound webhooks require the Linear webhook signing secret. The connector
checks `raw.signing_secret`, `raw.metadata.signing_secret`,
`binding.config.signing_secret`, `binding.config.secrets.signing_secret`,
`binding.config.secret_ids.signing_secret`, or managed-ingress secret aliases
in `raw.metadata.secret_ids` or `raw.metadata.config.secret_ids`; otherwise it
reads `linear/webhook-secret` through `secret_get`.

Outbound calls require one of:

- `api_token` or `api_token_secret` for the package setup secret. Requests use
  `Authorization: <token>`.
- `access_token` or `access_token_secret` for OAuth tokens. Requests use
  `Authorization: Bearer <token>`.
- `api_key` or `api_key_secret` for personal API keys. Requests use
  `Authorization: <api_key>`.

Use OAuth for workspace/user integrations and personal API keys only for local
scripts or private automation. Minimum OAuth scopes depend on the methods used:

- `read` for `list_issues`, `search`, and read-only `graphql` queries.
- `write` or narrower write scopes for mutations.
- `comments:create` is enough for `create_comment`.
- `issues:create` is only needed for workflows that create issues.
- Avoid `admin`; these helpers do not require it.

## Inbound Events

Supported Linear webhook resources are:

- `Issue` -> `linear.issue.<action>`
- `Comment` / `IssueComment` -> `linear.comment.<action>`
- `IssueLabel` -> `linear.issue_label.<action>`
- `Project` / `ProjectUpdate` -> `linear.project.<action>`
- `Cycle` -> `linear.cycle.<action>`
- `Customer` -> `linear.customer.<action>`
- `CustomerRequest` -> `linear.customer_request.<action>`

Actions are `create`, `update`, and `remove`. Unknown resources are normalized
with their lower-case resource name; unknown actions are rejected. Dedupe keys
prefer `linear:<Linear-Delivery>`, then `linear:<webhookId>`, then
`linear:sha256:<raw-body-hash>`.

## Outbound Methods

`call("graphql", args)` is the escape hatch. It accepts `query`, optional
`variables`, optional `operation_name` / `operationName`, auth, and optional
`api_base_url`.

Typed helpers are local to this package:

- `list_issues({ filter?, first?, after?, include_archived? })`
- `update_issue({ id, changes })`
- `create_comment({ issue_id, body })`
- `search({ query, first? })`

Connection-returning methods preserve Linear pagination data in `pageInfo`.
Callers should pass `pageInfo.endCursor` as `after` while
`pageInfo.hasNextPage` is true. `search` uses Linear's `searchIssues` field and
falls back to `term` when Linear returns a GraphQL validation error for the
`query` argument.

GraphQL responses always include `meta` on success. Typed helper return objects
also receive this `meta` field:

- `observed_complexity`
- `complexity_estimate` as a best-effort local hint
- `complexity_warning`
- `rate_limit.requests_*`
- `rate_limit.complexity_*`

GraphQL errors throw structured objects. Partial data is preserved on
`error.data`, GraphQL errors are preserved on `error.errors`, and rate-limit
responses are classified as `error.code == "rate_limited"` when Linear returns
HTTP 429 or a GraphQL error with `extensions.code == "RATELIMITED"`.

## Development

The connector exports `provider_id`, `kinds`, `payload_schema`, `init`,
`activate`, `shutdown`, `normalize_inbound`, and `call`. `normalize_inbound`
returns `NormalizeResult` v1:

- valid Linear webhooks return `{ type: "event", event: { kind, dedupe_key, payload } }`
- failed signature, missing timestamp, replay-window, and unsupported-action
  cases return `{ type: "reject", status, headers, body }`

Dedupe keys are deterministic. The connector prefers `Linear-Delivery`, then
`webhookId`, and finally a hash of the raw request body. Event kinds are
`linear.<resource>.<action>`, for example `linear.issue.update` and
`linear.comment.create`.

Run the local contract and fixture suite with:

```sh
harn --version
harn connector test "$(pwd)" --provider linear
```

That full package gate runs check, lint, format, package install/import smoke,
doc examples, the connector contract harness, and all `tests/*.harn` fixture
programs. Pass an absolute package path, as shown above, so the clean consumer
package smoke can resolve the path dependency from its temporary directory.

CI installs the pinned Harn CLI from `.harn-version` and runs the same full
package gate. Local development should use the installed CLI and this package's
`harn.toml`; it should not depend on a sibling `~/projects/harn` checkout.

The connector contract fixtures include a Harn Cloud managed-ingress delivery
with `metadata.secret_ids.signing_secret = "linear.webhook.secret"`. Harn Cloud
maps that alias when the connector calls `secret_get("linear/signing_secret")`,
so tests cover managed ingress without live provider credentials.

## License

Dual-licensed under MIT and Apache-2.0.

- [LICENSE-MIT](./LICENSE-MIT)
- [LICENSE-APACHE](./LICENSE-APACHE)
