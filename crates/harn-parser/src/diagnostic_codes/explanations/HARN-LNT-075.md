# HARN-LNT-075 — tool handler returns a freeform dict

A tool handler must declare its operation outcome independently of its data.
Freeform dictionaries don't declare that outcome, so dispatch rejects them
with `schema_validation`, including dictionaries returned by helpers or mutable
bindings.

That guess cannot be finished. A dict carrying a `status` key may be declaring
a failure or merely reporting progress, and no set of key names separates the
two, because the value carries no type saying which it is. A handler is equally
free to return `{failed: true}` or `{error_code: 7}`, which no convention
covers. Those shapes are now contract errors.

This is not hypothetical. A handler returning `{ok: false}` had its refusal
rendered to display text before anything classified it, and every dict-shaped
refusal was reported a success until `harn#7884` fixed the reader.

## How to fix

Return a typed struct. The type declares the outcome, so no reader has to infer
it:

```harn
struct ApplyOutcome {
  ok: bool,
  message: string,
}

fn apply_handler(args: dict) -> ApplyOutcome {
  if args.blocked {
    return ApplyOutcome{ok: false, message: "the rewrite was refused"}
  }
  return ApplyOutcome{ok: true, message: "applied"}
}
```

When the handler's result is text the model should read, return the handler
result envelope, which renders its `text` verbatim and carries structured data
beside it:

```harn
fn search_handler(args: dict) -> dict {
  return {
    schema: "harn.agent_tool_handler_result.v2",
    outcome: "ok",
    text: "3 matches",
    data: {matches: 3},
  }
}
```

The envelope requires `outcome`, `text`, and `data`. `outcome` accepts `"ok"`,
`"error"`, or `"rejected"`. `agent_tool_handler_result(text, data, outcome)`
constructs it; omitted `outcome` defaults to `"ok"`. Data fields never override
the declaration. Nominal structs must carry exactly one boolean `ok` or
`success` field; other fields don't decide the outcome.

## Severity

This is an error for a freeform dict literal returned directly from a tool
handler or through a same-body immutable binding. The checker
does not follow every mutable binding or helper-function return. Dispatch
validates their actual return values before rendering. A `handler` in a
different contract, such as a tool-search strategy, is outside this rule.
