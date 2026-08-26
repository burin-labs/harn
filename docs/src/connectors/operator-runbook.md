# Connector operator runbook

This is the release gate for Harn connector ingress. It validates the package
contract first, then the HTTP or daemon path that operators actually run.
Provider API mappings and credential names come from each package's
`harn.toml`; Harn owns ingress isolation, replay/dedupe, effect policy, OAuth
coordination, and runner supervision.

Never put secret values in a command argument, issue, transcript, or run
receipt. Store them through the configured secret provider and refer to the
package-declared secret id.

## Preflight

Record the Harn commit, connector tag, environment, and operator before each
validation. The generated [connector parity matrix](./parity-matrix.md) is the
checked source for package versions, Harn version floors, capabilities,
credential ids, setup commands, validation commands, and fixture coverage.

```bash
harn --version
harn config inspect --explain
harn check --connector-matrix --format=markdown
harn connect status --json
```

For every enabled connector, confirm:

- the installed Harn version satisfies the package floor
- only declared capabilities are enabled; connector capabilities default off
- `harn config inspect --explain` identifies the winning layer
- required secret ids are present without exposing their values
- normalization has no network, file, process, or LLM authority

## Provider setup

Install the connector tag shown in the parity matrix before running its setup
command. `harn connect setup-plan --connector <id> --json` is the
machine-readable source for the package's current secrets, scopes, health
checks, and recovery copy.

### Headless API-key setup

Use a package-declared environment variable on a server, CI runner, container,
or SSH session that has no unlocked keyring. Read the accepted names from the
setup plan, set one in the process that launches Harn, and check the result:

```bash
harn connect setup-plan --connector duffel --json
printf 'Duffel key: '
read -rs DUFFEL_TEST_KEY
printf '\n'
export DUFFEL_TEST_KEY
harn connect status --connector duffel --json
```

The variable must remain set in the service, job, or shell that runs Harn.
`--from-env NAME` has a different purpose: it copies a value into the configured
keyring. If that write fails, Harn names any environment variables declared for
the same secret. It never prints their values.

Run `harn doctor` when keyring storage should work. The `secret:keyring` check
writes, reads, and deletes a unique scratch entry. A locked or read-only store
therefore fails the check instead of appearing healthy.

### GitHub App and user OAuth

Create a GitHub App owned by the example organization. Set its webhook URL to
`https://<public-host>/webhooks/github`, generate a webhook secret, and subscribe
only to the events used by the trigger. Start with read-only Metadata, Issues,
Pull requests, and Contents permissions. Add the corresponding write permission
only for an enabled typed mutation. Actions runner reads require
`administration:read` (repository) or
`organization_self_hosted_runners:read` (organization); JIT registration and
cleanup require the corresponding write permission.

Download the App private key once into a permission-restricted temporary file,
then let Harn move it into the secret provider:

```bash
harn connect github --app-slug harn-example --app-id 12345 \
  --private-key-file ./harn-example.private-key.pem \
  --webhook-secret-file ./github-webhook-secret --json
harn connect status --connector github --json
```

The stored references are `github/app-private-key` and
`github/webhook-secret`; the manifest refers to those ids, never PEM or secret
values. Delete the temporary files after `status` reports `healthy`. To rotate,
add a new App key, rerun setup with that key, prove one signed webhook and one
typed read, then revoke the old key in GitHub. Rotate the webhook secret by
updating GitHub and the secret provider in one maintenance window and replaying
the invalid-old/valid-new fixture pair.

GitHub user OAuth is separate from App installation authentication. Use the
connector's `oauth_user_device_code` and `oauth_user_access_token` typed methods
with the narrow requested scopes; do not substitute a personal access token.
For concurrent clients, run the single-flight validation below before enabling
token rotation.

### Slack Events API and Socket Mode

Create a Slack app from the version-pinned manifest in the
[`harn-slack-connector` README](https://github.com/burin-labs/harn-slack-connector).
For HTTP delivery, enable Events API and set the Request URL to
`https://<public-host>/webhooks/slack`. Store the signing secret as
`slack/signing-secret`, authorize with `harn connect slack`, and store the bot
token as `slack/bot-token`.

Minimum scopes are determined by enabled methods: `app_mentions:read`,
`channels:history`, `groups:history`, `im:history`, or `mpim:history` for the
corresponding events; `chat:write` for messages; `reactions:write` for
reactions; `users:read` for user lookup; and `files:write` for uploads. Private
channel reads require the matching `groups:*` scope and app membership in that
channel. Validate that a non-member request returns a permission diagnostic and
does not fall back to broader history.

Socket Mode is off by default. To opt in, create an app-level token with only
`connections:write`, store it as `slack/app-token`, and set
`socket_mode_enabled: true` in the package binding. Rotation is add-new,
reconnect-and-ack-one-envelope, then revoke-old. HTTP signing-secret rotation
uses the same invalid-old/valid-new proof as GitHub.

```bash
harn connect slack --scope \
  app_mentions:read,channels:history,chat:write --json
harn connect status --connector slack --json
```

### CircleCI

In Project Settings, create a webhook at
`https://<public-host>/webhooks/circleci`, select workflow-completed and
job-completed, and configure a secret. Store it as
`circleci/webhook-secret`. Create a personal API token only when typed reads or
reruns are enabled; store it as `circleci/api-token`. The API token has the
authority of its CircleCI user, so use a dedicated least-authority user with
access only to the example project.

```bash
harn connect api-key --connector circleci \
  --secret-id circleci/webhook-secret
harn connect api-key --connector circleci --secret-id circleci/api-token
harn connect status --connector circleci --json
```

Rotate each value by storing the replacement through the same prompt, updating
the CircleCI webhook or user token, proving a signed failure event and typed
workflow read, then revoking the old value.

### Buildkite

Create an organization webhook at
`https://<public-host>/webhooks/buildkite` for `build.finished` and
`job.finished`. Prefer the timestamped HMAC signature mode and store its token
as `buildkite/webhook-token`. Create a dedicated API token with
`read_builds` for build/job/log reads and add `write_builds` only when retry,
rebuild, or cancel actions are enabled; store it as `buildkite/api-token`.
GraphQL access is separately powerful and must remain disabled unless a typed
method requires it.

```bash
harn connect api-key --connector buildkite \
  --secret-id buildkite/webhook-token
harn connect api-key --connector buildkite --secret-id buildkite/api-token \
  --scopes read_builds
harn connect status --connector buildkite --json
```

Rotate webhook and API tokens independently. A successful rotation proves one
new signed event, one typed build read, and rejection of the old credential
before revocation is recorded complete.

### Bitbucket and secondary forges

For Bitbucket Cloud, configure the repository webhook at
`https://<public-host>/webhooks/bitbucket`, store its HMAC secret as
`bitbucket/webhook-secret`, and use a repository- or workspace-scoped access
token as `bitbucket/api-token`. Grant repository read plus pull-request or
commit-status write only when those typed mutations are enabled. Data Center
uses the equivalent instance PAT and callback path. The provider README named
by the parity matrix owns the exact event headers and scope mapping.

### Secret handling check

After every setup or rotation, send a neutral fixture containing sentinel
values that resemble the secret ids, inspect the trigger record, and search the
state directory for the sentinel. The secret value itself must never be used as
the sentinel or printed:

```bash
harn connect status --connector github --json
harn orchestrator inspect --state-dir ./.harn/orchestrator --json
rg 'github/app-private-key|github/webhook-secret' .harn
```

The last command may find references; it must not find credential values.

## Deterministic package gate

Run the package gate from a clean checkout of each connector tag listed in the
matrix. These commands exercise the package-owned verifier and normalizer
against signed, invalid-signature, stale/replay, dedupe, and action-policy
fixtures declared by that package.

```bash
harn package verify . --provider github
harn package verify . --provider slack
harn package verify . --provider linear
harn package verify . --provider notion --run-poll-tick
harn package verify . --provider circleci
harn package verify . --provider buildkite
harn package verify . --provider bitbucket
```

Do not waive a missing negative fixture. Each webhook package must demonstrate
one accepted authenticated event, one authentication failure, one stale or
replayed request rejection when the provider supplies a timestamp, and a
stable dedupe key. Poll and streaming packages must demonstrate cursor/replay
behavior at their transport boundary.

## Canonical ingress gate

Create a disposable workspace that pins the connector tag and declares a
provider plus trigger binding. Store the declared secrets, start the
orchestrator on loopback, and send the package's signed fixture through the
actual listener route.

```bash
harn connect setup-plan --connector github --json
harn connect status --connector github --json
harn orchestrator serve --state-dir ./.harn/orchestrator --public-metrics
harn orchestrator inspect --state-dir ./.harn/orchestrator --json
```

Repeat the listener test with the invalid signature, stale timestamp, and
duplicate delivery. The required observations are:

| Probe | HTTP result | Dispatch result |
|---|---|---|
| Valid signed delivery | Provider-defined success | One inbox event and one dispatch |
| Invalid signature/auth | `401` or provider-defined rejection | No inbox event |
| Stale/replayed delivery | `401` or `409` | No inbox event |
| Duplicate delivery id | Success or `409`, consistently | Exactly one dispatch |
| Normalizer attempts network/file/process/LLM | Rejection | No side effect and no dispatch |

Slack URL verification is the one immediate-response route: it must return the
challenge only after Slack request authentication succeeds and must not enqueue
an event. Notion polling must additionally prove that a restarted poll resumes
from the committed cursor without re-emitting the same dedupe key.

## Action and OAuth gate

For every action-capable package, exercise one read and one mutation using the
typed connector method. The read must require only its declared network and
secret capabilities. The mutation must produce an approval/effect-policy
decision and an audit receipt; a denied mutation must make no provider call.
Raw API escape hatches do not substitute for typed least-authority methods.

For OAuth connectors, run the deterministic concurrent-refresh regression
before live validation. In a disposable provider account, then launch two
clients against one expired credential and confirm one refresh occurs, both
clients reread the committed credential, and neither persists a stale rotated
refresh token.

The checked local regression is:

```bash
harn test conformance --filter oauth_client_refresh_singleflight_reread
```

For CI failure handling, validate these end-to-end outcomes:

| Scenario | Typed read | Mutation | Required policy result |
|---|---|---|---|
| GitHub event | issue, pull request, check, or workflow read | one matching typed action | explicit approval before provider call |
| CircleCI failed job | workflow/job lookup and artifact list | rerun from failed | denied action makes zero HTTP calls |
| Buildkite failed job | build/job lookup and log fetch | job retry or rebuild | `write_builds` plus explicit approval |

Retry only from the persisted event or action receipt. A blind raw-API retry is
not evidence because it bypasses the typed contract, dedupe key, and audit
lineage.

## Ephemeral runner gate

GitHub Actions just-in-time runners use `std/runner_pool`. Provider code mints
the JIT configuration and supplies the adapter closure; the shared pool owns
supervision, replacement, scaling, graceful drain, and online/busy state.

Validate:

- pool size remains within the declared minimum and maximum
- every slot invokes the runner with a single-use JIT configuration
- a completed or failed runner is deregistered and replaced according to the
  shared restart policy
- online/busy transitions appear in `runner_pool_state`
- scaling drains the old generation before the new generation starts
- shutdown leaves no registered runner or child process

The shared lifecycle proof is public and credential-free:

```bash
harn test conformance --filter runner_pool_lifecycle
```

For a live JIT proof, enable the GitHub package's typed runner methods, generate
one single-use JIT configuration, and pass the resulting adapter closure to
`runner_pool_start`. Record the allocation id, online/busy transitions, job
receipt, process exit, deregistration, and replacement or terminal stop. Local
runner mode uses the same pool contract but returns a local process adapter
instead of minting GitHub registration material.

## Observability and recovery

```bash
harn connect status --json
harn orchestrator inspect --state-dir ./.harn/orchestrator --json
curl --fail http://127.0.0.1:8080/metrics
```

Correlate the delivery id across the ingress receipt, trigger inbox,
`observability.action_graph`, dispatch result, and provider action receipt.
Check queue age, dispatch latency, retries, backpressure, dedupe, signature
rejections, and DLQ counters. If OpenTelemetry is enabled, confirm the same
trace reaches the configured collector.

Recovery is package-driven:

1. Run `harn connect status --connector <id> --json`.
2. Follow the package's recovery copy for `missing_auth`,
   `expired_credentials`, `revoked_credentials`, `missing_scopes`,
   `inaccessible_resource`, or `transient_provider_outage`.
3. Rotate or revoke credentials only under the environment's credential
   procedure.
4. Rerun the deterministic package gate and canonical ingress gate.
5. Record the new credential version identifier, never its value.

Stable `harn connect status --json` states:

| Status | Meaning | Recovery |
|---|---|---|
| `healthy` | Required secrets, expiry, scopes, and health checks passed. | Continue with the package and canonical ingress gates. |
| `missing_auth` | A required secret or OAuth record is absent. | Run the package setup command and store the declared secret ids. |
| `expired_credentials` | The stored credential is past its expiry. | Refresh or reconnect; do not retry provider actions first. |
| `revoked_credentials` | The credential index exists but its backing secret is gone or revoked. | Revoke the stale local record, reconnect, then validate. |
| `missing_scopes` | Authentication works but required declared scopes are absent. | Reauthorize with only the reported missing scopes. |
| `inaccessible_resource` | Credentials are valid but the target resource is not reachable. | Grant resource access or correct the resource id. |
| `transient_provider_outage` | Provider or credential-backend health could not be established. | Preserve the event and retry after the dependency recovers. |

Ingress rejection and denied-effect diagnostics must include the provider,
delivery/dedupe id when available, stable status or diagnostic code, and trace
id. They must not include raw authorization headers, signing secrets, private
keys, OAuth refresh tokens, or package secret values.

## Capability-change review checklist

Update this checklist in the same pull request as any connector capability:

- package version and Harn floor match the tagged connector
- provider capability, setup, scope, secret, health, and recovery metadata are
  updated in `harn.toml`
- the generated parity matrix is regenerated and drift-checked
- package README documents creation, callback, least scopes, rotation, and the
  exact `harn package verify` command
- valid, invalid-auth, stale/replay, dedupe, disabled-by-default, missing-scope,
  expired-credential, and denied-effect fixtures cover the affected route
- one typed read and every new mutation have effect-policy and zero-call denial
  assertions
- HTTP, polling, streaming, or Socket Mode canonical ingress is exercised as
  applicable
- event, dispatch/retry/DLQ, action receipt, and cleanup share a correlation id
- local/JIT runner changes use `std/runner_pool` and prove graceful cleanup
- all documented commands and flags pass the docs CLI checker

The cutover is releasable only when every enabled row has terminal evidence for
its deterministic package gate, canonical route, negative security probes,
action policy (when applicable), and recovery path.
