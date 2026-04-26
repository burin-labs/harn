# GitHub App connector

> **Deprecated.** The Rust-side `GitHubConnector` is a compatibility shim under
> the [#350](https://github.com/burin-labs/harn/issues/350) pure-Harn connector
> pivot. The recommended implementation is the pure-Harn
> [`harn-github-connector`](https://github.com/burin-labs/harn-github-connector)
> package. This page documents the existing Rust connector for users who have
> not yet migrated; new deployments should configure the Harn package via
> `connector = { harn = "..." }` on the `[[providers]]` table. See the
> [Rust-to-Harn-package migration guide](../migrations/rust-connectors-to-harn-packages.md)
> for a no-downtime cutover path.

`GitHubConnector` is Harn's built-in GitHub App integration for inbound webhook
events plus outbound GitHub REST calls authenticated as an installation.

The MVP scope in `#170` is intentionally narrow:

- inbound GitHub webhook verification with `X-Hub-Signature-256`
- strongly typed payload narrowing for the monitor-relevant event families:
  `issues`, `pull_request`, `issue_comment`, `pull_request_review`, `push`,
  `workflow_run`, `deployment_status`, and `check_run`
- outbound installation-token lifecycle for GitHub App auth
- eight outbound methods exposed through `std/connectors/github`, including a
  raw REST escape hatch

Guided install / OAuth setup remains deferred to C-10. This landing supports
the manual-config path now: provide the App id, installation id, private key,
and webhook secret through the orchestrator config + secret providers.

## Inbound webhook bindings

Configure GitHub as a `provider = "github"` webhook trigger:

```toml
[[triggers]]
id = "github-prs"
kind = "webhook"
provider = "github"
match = { path = "/hooks/github" }
handler = "handlers::on_github"
dedupe_key = "event.dedupe_key"
secrets = { signing_secret = "github/webhook-secret" }
```

The connector verifies `X-Hub-Signature-256` against the raw request body using
the shared `verify_hmac_signed(...)` helper from the generic webhook path. It
does not duplicate HMAC logic. Successful deliveries normalize into
`TriggerEvent` with:

- `kind` from `X-GitHub-Event`
- `dedupe_key` from `X-GitHub-Delivery`
- `signature_status = { state: "verified" }`
- `provider_payload = GitHubEventPayload`

`GitHubEventPayload` is narrowed into eight monitor-relevant event families.
For example, an `issues` delivery exposes `payload.issue`,
`pull_request_review` exposes both `payload.review` and
`payload.pull_request`, `deployment_status` exposes
`payload.deployment_status` plus `payload.deployment`, and `check_run`
exposes `payload.check_run`.

## Outbound configuration

Outbound helpers authenticate as a GitHub App installation. Required config:

- `app_id`
- `installation_id`
- `private_key_pem` or `private_key_secret`

Optional config:

- `api_base_url` for GitHub Enterprise or tests; defaults to
  `https://api.github.com`

Recommended production shape:

```harn
import { configure } from "std/connectors/github"

configure({
  app_id: 12345,
  installation_id: 67890,
  private_key_secret: "github/app-private-key",
})
```

For tests and local fixtures, `private_key_pem` can be passed inline.

## Installation-token lifecycle

The connector follows the GitHub App installation flow:

1. Mint a short-lived App JWT (`RS256`, `iss = app_id`) from the configured
   private key.
2. Exchange it at `POST /app/installations/{installation_id}/access_tokens`.
3. Cache the returned installation token per installation.
4. Refresh lazily a little before expiry, or immediately after a `401`.

The in-process cache refreshes roughly every 55 minutes even though GitHub
tokens are valid for one hour. Token fetches still flow through the shared
secret-provider-backed connector context, and outbound requests are scoped
through the connector `RateLimiterFactory`.

## Outbound helpers

Import from `std/connectors/github`:

```harn
import {
  add_labels,
  api_call,
  check_run_source,
  comment,
  create_issue,
  deployment_status_source,
  get_pr_diff,
  list_stale_prs,
  merge_pr,
  pull_request_merged_source,
  request_review,
  wait_until_ci_green,
  wait_until_deploy_succeeds,
  wait_until_pr_merged,
} from "std/connectors/github"
```

Available methods:

- `comment(issue_url, body, options = nil)`
- `add_labels(issue_url, labels, options = nil)`
- `request_review(pr_url, reviewers, options = nil)`
- `merge_pr(pr_url, options = nil)`
- `list_stale_prs(repo, days, options = nil)`
- `get_pr_diff(pr_url, options = nil)`
- `create_issue(repo, title, body = nil, labels = nil, options = nil)`
- `api_call(path, method, body = nil, options = nil)`
- `deployment_status_source(repo, deployment_id, options = nil)`
- `check_run_source(repo, check_run_id, options = nil)`
- `pull_request_merged_source(repo, number, options = nil)`
- `wait_until_deploy_succeeds(repo, deployment_id, options = nil)`
- `wait_until_ci_green(repo, check_run_id, options = nil)`
- `wait_until_pr_merged(repo, number, options = nil)`

All helpers accept the same auth/config fields through `options`, but
`configure(...)` is the intended shared setup path.

Example:

```harn
import {
  comment,
  configure,
  list_stale_prs,
  merge_pr,
} from "std/connectors/github"

pipeline default() {
  configure({
    app_id: 12345,
    installation_id: 67890,
    private_key_secret: "github/app-private-key",
  })

  let stale = list_stale_prs("acme/api", 14)
  if stale.total_count > 0 {
    let pr = stale.items[0]
    comment("https://github.com/acme/api/issues/" + to_string(pr.number), "Taking a look.")
  }

  let merged = merge_pr(
    "https://github.com/acme/api/pull/42",
    {merge_method: "squash", admin_override: true},
  )
  println(merged.merged)
}
```

`admin_override: true` records that the caller requested an override and
annotates the returned JSON with `admin_override_requested = true`. GitHub's
REST merge endpoint does not currently expose a distinct override flag, so the
connector still uses the standard merge call.

`api_call(...)` is the JSON-oriented escape hatch for installation-authenticated
REST endpoints that do not yet have a dedicated helper. It accepts a relative
API `path`, an HTTP `method`, an optional JSON `body`, and the usual auth
options. Pass `options.accept` when a GitHub preview or alternate JSON media
type is required.

The monitor source helpers return `std/monitors` source dicts. They poll GitHub
REST for authoritative state and set `prefers_push = true` so inbound
`deployment_status`, `check_run`, and `pull_request` webhooks can wake the
monitor before the next poll interval. The `wait_until_*` helpers wrap those
sources with common success conditions; pass monitor options such as `timeout`
and `poll_interval` alongside the usual GitHub auth options.

## Rate limiting

The connector uses the shared `RateLimiterFactory` with a per-installation
scope key before each outbound request. It also reacts to GitHub rate-limit
responses:

- retries once after `429` using `Retry-After` or `X-RateLimit-Reset`
- invalidates cached tokens and re-mints on `401`
- emits observations to the `connectors.github.rate_limit` event-log topic

This keeps the MVP aligned with the generic connector rate-limit contract
without introducing a second bespoke limiter.
