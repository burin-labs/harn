# Connector operator runbook

This is the repo-local first slice for operators standing up connector dogfood
scenarios. It intentionally records only what this repository already exposes
or what issue #2952 names as operator-owned placeholders. Provider-specific
setup details still belong in the connector package repos and the living
operator runbook.

## Static credential inventory

Use `harn connect status --json` and
`harn connect setup-plan --connector <id> --json` from the workspace that owns
the relevant `harn.toml`. Those commands are the local source of truth for the
connector ids, required secrets, scopes, and recovery copy declared by packages.

| Surface | Operator must mint or decide | Repo-local destination |
|---|---|---|
| GitHub App | Create a GitHub App for bot-authored actions. Grant only the permissions needed by the dogfood path, starting from pull requests, issues, contents metadata/read, checks, Actions, and administration or organization self-hosted runners when runner management is in scope. Generate a private key, choose a webhook secret, and install the App on the org or target repos. | Current CLI storage is `github/installation-<id>`, `github/app-<app-id>/private-key`, and `github/webhook-secret` via `harn connect github`. Issue #2952 also names `github/app-private-key`; treat that as an operator placeholder until the implementation standardizes it. |
| GitHub OAuth | Create a user-to-server OAuth app. Record the client id/secret, callback URL, device-flow setting, and expiring-token toggle. | This repo documents generic OAuth and GitHub App setup, but no dedicated repo-local GitHub OAuth connector command. Store the client details in provider metadata or the external operator secret store until the connector package declares exact ids. |
| Slack | Create a Slack app. Choose Events API or Socket Mode. For Events, record the signing secret and request URL. For Socket Mode, create an app-level `xapp-` token with `connections:write`. Request only the needed scopes, such as `app_mentions:read`, `channels:history`, `groups:history`, `im:history`, `mpim:history`, `chat:write`, and `assistant:write`. Invite the bot user to any private channel used in dogfood. | `harn connect slack` stores OAuth material under Slack provider token ids. Manifest examples use `slack/signing-secret` for request signing. Socket Mode token ids are connector-package placeholders until declared in setup metadata. |
| CircleCI | Create a project webhook, capture its signing secret, and mint a `Circle-Token` API token for outbound diagnose/rerun calls. | The catalog documents `circleci-signature` verification and `Circle-Token` outbound auth. Exact secret ids should come from the CircleCI connector package `required_secrets`; do not assume a core id. |
| Buildkite | Create a webhook token for HMAC verification and mint an API access token with the narrowest scopes needed for build lookup, log fetch, retry, rebuild, and cancel paths. | The catalog documents `X-Buildkite-Signature` and `X-Buildkite-Token` verification plus bearer-token outbound auth. Exact secret ids should come from the Buildkite connector package `required_secrets`; do not assume a core id. |
| GitHub Actions runners | Decide whether dogfood uses persistent registration or just-in-time runner registration. Record where the runner binary runs, what labels it advertises, how registration credentials are supplied, and how removal is performed. | No repo-local Harn runner-management command is documented here yet. Track this as an operator placeholder next to the GitHub App permission set. |
| Cloud kill-switches and plan tiers | For each hosted connector reaction, record the exact `HARN_CLOUD_*_ENABLED` flag, default state, owning team, plan gate, and emergency rollback owner. | This repo does not currently define concrete `HARN_CLOUD_*_ENABLED` flags for connectors. Keep placeholders out of checked-in config until a cloud platform publishes the names. |

Before dogfood starts, save a credential worksheet with:

- provider and environment
- app id, installation id, workspace/team/org id, or project slug
- secret id or external-vault path
- scope or permission grant
- rotation owner and expiration
- rollback or revoke steps

Do not paste secret values into issues, PRs, transcripts, or run receipts.

## Manual dogfood checklist

Run these in a staging org/workspace first. Capture the Harn commit, connector
package revision, Burin build, and whether the run used local or cloud dispatch.

- [ ] Run `harn config inspect --explain` and confirm the expected config layer
  wins. Managed locks or denied fields should be understandable without reading
  raw TOML.
- [ ] Run `harn connect status --json` in the test workspace. Every connector
  under test should be `healthy` or have an expected remediation such as
  `missing_auth` before setup.
- [ ] Run `harn connect setup-plan --connector <id> --json` for any connector
  that is not usable and follow only commands or setup text emitted by the
  package metadata.
- [ ] Connect a GitHub account in Burin and confirm repo data plus worktrees
  appear.
- [ ] Mention the agent in one public Slack channel, one private channel where
  the bot was invited, and one DM. Repeat the path for local dispatch and cloud
  dispatch when both are available.
- [ ] Open one GitHub PR with a merge conflict and one PR with a failing check.
  Confirm the reaction, writeback identity, and audit receipt match the
  configured GitHub App installation.
- [ ] Trigger one CircleCI failure and one Buildkite failure. Confirm diagnose,
  log/artifact lookup, and rerun or retry behavior only when the connector
  status reports the required credentials.
- [ ] Register or spawn the selected local/JIT Actions runner and verify it
  picks up exactly the expected job labels. Remove it after the dogfood run.
- [ ] Sign into two Burin IDEs and confirm shared connections work without
  refresh-token loss or unexpected reauthorization.
- [ ] Revoke one non-production credential and confirm `harn connect status
  --json` reports the expected degraded state before reconnecting.

## Observability and status commands

Start with local surfaces that are already documented in this repository:

```bash
harn config inspect --explain
harn connect status --json
harn connect status --connector github --json
harn connect setup-plan --connector github --json
harn orchestrator inspect --state-dir ./.harn/orchestrator --json
curl http://127.0.0.1:8080/metrics
```

Use `harn config inspect --explain` to answer:

- Which config layer supplied the winning value?
- Which managed fields are locked?
- Which later values were shadowed or denied?
- Are the visible values redacted where they look like secrets?

Use `harn connect status --json` to answer:

- Which manifest was loaded?
- Is each connector installed?
- Is it usable?
- Is the status `healthy`, `missing_install`, `missing_auth`,
  `expired_credentials`, `revoked_credentials`, `missing_scopes`,
  `inaccessible_resource`, or `transient_provider_outage`?
- Which `required_secrets`, `required_scopes`, and recovery messages did the
  connector package declare?

For trigger and dispatch observability, correlate these surfaces:

- `harn orchestrator inspect --state-dir <dir> --json` for local orchestrator
  trigger, queue, budget, and recent dispatch state.
- The `/metrics` endpoint for ingress, queue age, dispatch latency, retry,
  backpressure, and DLQ counters.
- The `observability.action_graph` event-log topic and portal run detail for
  the timeline from inbox to predicate to dispatch to A2A to worker to DLQ.
- OpenTelemetry export with `HARN_OTEL_ENDPOINT`, `HARN_OTEL_SERVICE_NAME`,
  `HARN_OTEL_HEADERS`, and `HARN_OTEL_SAMPLE_RATIO` when a collector is
  configured.

Cloud-specific metrics, receipts for ingress verify/deny, dedupe, dispatch,
the hosted OTel waterfall, and the IDE `/connectors` panel are operator
placeholders in this repo-local slice. Link them back here once the exact
cloud and IDE-host surfaces are published.

## Placeholder policy

When a provider path is not implemented or not named in this repository:

- keep the provider row as an operator placeholder
- record the external system that owns the decision
- avoid inventing `harn` commands, secret ids, env vars, or dashboard URLs
- update this page only after the command, id, flag, or URL exists in code,
  connector package metadata, or the published operator runbook
