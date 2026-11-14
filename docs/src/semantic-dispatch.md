# Route requests by meaning

Use `std/decision` to bind natural-language meanings to typed handlers. Harn
checks the input, asks a native decision model two questions, and invokes one
handler when your acceptance policy passes.

Replay the included lookup from the repository root without provider access:

```sh
HARN_LLM_CALLS_DISABLED=1 harn run examples/semantic-dispatch/run.harn \
  --evaluation-tape examples/semantic-dispatch/lookup.tape \
  -- '{"order_id":"A42","message":"Where is my order?"}'
```

The reply names the `lookup` handler. Accounting reports `source: "tape"` and
zero physical attempts. Changing the request makes replay fail. Evaluation
replay still invokes handlers; this lookup uses only synthetic data.

Start the repository's synthetic order service from the repository root:

```sh
harn serve site examples/semantic-dispatch/server.harn --bind 127.0.0.1:8080
```

Choose an available native route with `harn models recommend --operation decision`.
The example uses `openrouter/typesafe/jev-1.14` and requires its provider credential.
Change `policy()` in [the example](../../examples/semantic-dispatch/app.harn) to
choose another native route. Requests send their input and route meanings to
that provider. Use synthetic data until its data controls meet your requirements.

Send a request:

```sh
curl --fail-with-body http://127.0.0.1:8080/orders \
  -H 'Content-Type: application/json' \
  -d '{"order_id":"A42","message":"Where is my order?"}'
```

An accepted request returns `kind: "dispatched"`, `value.handler: "lookup"`, and
the evaluation receipt reference. A cancellation request prepares a review;
it doesn't change an order. A delivery dispute starts a bounded agent with one
tool that reads a synthetic delivery record.

Handle the other HTTP responses explicitly:

| Status | Meaning | Next action |
| --- | --- | --- |
| 400 | Invalid JSON or input shape | Correct the request |
| 409 | Insufficient probability or margin | Ask for clarification or review |
| 422 | No supported action fits | Offer another service |
| 502 | Investigation agent didn't finish normally | Inspect `agent_terminal` and retry or review |
| 503 | Evaluation failed | Inspect the typed evaluation outcome before retrying |

An evaluation failure preserves cancellation, budget, refusal, transport, and
replay outcomes. A handler failure propagates to the host. Dispatch doesn't
retry handlers or select a second handler after an error.

Define your own router with `router<T, R>`. Supply a `SchemaContract<T>`, a stable
ID, shared instructions, a route list, and a `DecisionPolicy`. Each route contains
its name, meaning, minimum probability, and `fn(Harness, T, DecisionEvidence) -> R`
handler. All handlers return the same type `R`.

Use `assess(harness.llm, input)` to inspect a selection without executing a handler.
Use `dispatch(harness, input)` when selection should invoke the handler. Both use
the same definition and the existing evaluation budget, receipt, fixture, and
[replay](./decision-replay.md) mechanisms.

Keep authorization and irreversible changes behind ordinary deterministic
checks. A selected handler receives the same harness authority as its caller.
The decision doesn't grant capabilities or prove permission.

Before choosing production thresholds, measure wrong selections and abstentions
on representative labeled inputs. The example thresholds are demonstration
policy. Read [what confidence means](./concepts/confidence.md) and the
[dispatch reference](./stdlib/decision.md).
