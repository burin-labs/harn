# Multi-tenant orchestrator

Harn can run the orchestrator listener in a tenant-aware mode:

```bash
harn orchestrator tenant create acme --state-dir .harn/orchestrator
harn orchestrator serve --role multi-tenant --state-dir .harn/orchestrator
```

Tenant records live under `<state-dir>/tenants/registry.json`. Each tenant gets:

- a state root at `<state-dir>/tenants/<tenant-id>/`
- a secret namespace named `harn.tenant.<tenant-id>`
- event-log topics prefixed with `tenant.<tenant-id>.`
- an initial API key mapped to that tenant
- optional daily/hourly budget metadata and an ingest rate limit

Tenant ids may contain ASCII letters, numbers, `_`, and `-`.

## Request Resolution

In multi-tenant mode every inbound trigger request must resolve to a tenant. The preferred mechanism
is a tenant API key in `X-API-Key` or `Authorization: Bearer <key>`. Path-scoped ingress is also
supported for webhook routing:

```text
/hooks/tenant/<tenant-id>/<configured-trigger-path>
/tenant/<tenant-id>/<configured-trigger-path>
```

If both an API key and path tenant are present, they must name the same tenant. A mismatch returns
`403` and appends `tenant_access_denied` to `orchestrator.tenant.audit`. Suspended tenants return
`402` with state preserved.

## Isolation

Tenant-scoped ingress attaches `tenant_id` to normalized trigger events. Pending and inbox EventLog
records for those events are written to tenant-prefixed topics such as:

```text
tenant.acme.orchestrator.triggers.pending
tenant.acme.trigger.inbox.envelopes
tenant.acme.trigger.outbox
tenant.acme.trigger.attempts
tenant.acme.trigger.dlq
```

The runtime also exposes `TenantEventLog`, which transparently prefixes unscoped topic names and
rejects attempts to append or read another tenant's `tenant.<id>.` topic.

Signing secrets are loaded through a tenant-scoped provider in multi-tenant requests. A trigger that
references `github/webhook-signing-secret` resolves that name inside `harn.tenant.<tenant-id>`; an
explicit `harn.tenant.<other-id>/...` lookup is rejected.

## Tenant Lifecycle

```bash
harn orchestrator tenant ls --state-dir .harn/orchestrator
harn orchestrator tenant suspend acme --state-dir .harn/orchestrator
harn orchestrator tenant delete acme --confirm --state-dir .harn/orchestrator
```

`delete` removes the tenant registry entry and its state directory. `suspend` keeps state intact and
causes future ingress for that tenant to return `402`.
