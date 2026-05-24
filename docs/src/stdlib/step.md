# Durable step stdlib

`step.run(key, input?, handler, options?)` memoizes a handler result in Harn's
active EventLog. On a later process restart or script replay, Harn re-executes
the program from the top, but a matching step returns the persisted result
without invoking the handler again.

Use it around side-effectful or expensive work that must happen at most once
for a deterministic input.

```harn,ignore
fn main(harness: Harness) {
  let user = step.run("load-user", {user_id: harness.event.id}, { input ->
    return load_user(input.user_id)
  }, {namespace: "signup-workflow"})

  let enriched = step.run("enrich", {user_id: user.id}, { ->
    return call_enrichment(user)
  }, {namespace: "signup-workflow"})

  return enriched
}
```

## Contract

- `key` is the authored step name.
- The optional `input` is hashed and persisted as the deterministic input for
  that step occurrence.
- The first run invokes `handler`, persists the JSON-shaped result, and returns
  it.
- A replay with the same `key`, occurrence number, namespace, and input hash
  returns the persisted result without invoking `handler`.
- A replay with the same `key` and occurrence number but a different input hash
  throws a deterministic input mismatch error.

Repeated uses of the same key are tracked by occurrence number in execution
order. Keep keys stable and do not branch around earlier `step.run` calls unless
the branch condition is itself deterministic for the namespace.

## Namespaces

Pass `options.namespace` when the same script can run for independent logical
workflow instances:

```harn,ignore
let result = step.run("charge-card", {order_id: order.id}, { input ->
  return charge_order(input.order_id)
}, {namespace: "order-" + order.id})
```

If omitted, Harn uses the entry source path as the namespace. That is convenient
for local scripts, but production workflows should supply an event, session, or
order scoped namespace explicitly.

## Inspection

`step.inspect(namespace_or_options?)` returns completed records from the active
step log:

```harn,ignore
let records = step.inspect({namespace: "order-123"})
log(json_stringify(records))
```

Each record includes `event_id`, `namespace`, `key`, `sequence`,
`deterministic_inputs_hash`, `result`, and `occurred_at_ms`.

## Persistence and Privacy

Step records are EventLog entries on topic `step.run.<sanitized namespace>`;
the raw namespace is also persisted in each record and participates in replay
identity. Under `harn run`, records use the same `.harn` state root as store,
checkpoints, triggers, and waitpoints. The default EventLog backend is SQLite
unless `HARN_EVENT_LOG_BACKEND` overrides it.

Inputs and results are persisted. Do not pass secrets or raw private payloads as
step inputs/results unless the selected EventLog storage is allowed to hold
them. Persist hashes or stable opaque ids when that is enough for replay.
