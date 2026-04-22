# A2A push connector

`a2a-push` receives Agent2Agent push-notification webhooks from remote
A2A agents. It is for federated Harn orchestrators: one orchestrator can
start long-running work on another and be woken when the remote task
changes state.

```text
Orchestrator A  -- a2a://B/task -->  Orchestrator B
     ^                                    |
     |                                    |
     +------- push POST task done --------+
```

## Manifest

Use `kind = "a2a-push"` and add `[triggers.a2a_push]` to opt into the
connector verifier:

```toml
[[triggers]]
id = "reviewer-task-update"
kind = "a2a-push"
provider = "a2a-push"
path = "/a2a/review"
match = { events = ["a2a.task.completed", "a2a.task.failed"] }
handler = "handlers::on_reviewer_update"

[triggers.a2a_push]
expected_iss = "reviewer.prod"
expected_aud = "https://orchestrator.example.com/a2a/review"
jwks_url = "https://reviewer.prod/.well-known/jwks.json"
expected_token = "opaque-task-token"
```

JWT/JWKS is the default. The connector verifies:

- `Authorization: Bearer <jwt>` is present.
- The JWT signature matches the `kid` in the remote JWKS.
- `iss` and `aud` match the manifest.
- `exp` is still in the future and `iat` is not in the future.
- `jti` is single-use through the trigger inbox.
- `expected_token`, when set, matches the JWT `token` claim, the
  `X-A2A-Token` header, or a `token` field in the JSON body.

JWKS entries are cached for one day and refreshed after expiry.

## Event Shape

The connector accepts A2A stream-response push payloads:

```json
{
  "statusUpdate": {
    "taskId": "task-123",
    "contextId": "ctx-123",
    "status": {"state": "completed"}
  }
}
```

Task status updates map to `TriggerEvent.kind` values like
`a2a.task.completed`, `a2a.task.failed`, and `a2a.task.canceled`.
Artifact and message pushes map to `a2a.task.artifact` and
`a2a.task.message`.

Handlers receive `provider_payload` with:

- `task_id`
- `task_state`
- `artifact`
- `sender`
- `kind`
- `raw`

## A2A Dispatch Registration

When `HARN_A2A_PUSH_URL` is set, outbound `a2a://...` dispatches include
and register a `pushNotificationConfig` for pending tasks. Set
`HARN_A2A_PUSH_TOKEN` to include a bearer credential in that config.

The built-in `harn serve a2a` adapter advertises push notification
support, stores push configs, and posts `statusUpdate` payloads to each
configured webhook when an asynchronous task completes or fails.

Legacy `a2a-push` routes without `[triggers.a2a_push]` keep the older
orchestrator listener auth: `HARN_ORCHESTRATOR_API_KEYS` plus
`HARN_ORCHESTRATOR_HMAC_SECRET`.
