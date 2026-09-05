# HARN-LNT-075 — tool handler returns a freeform dict

A tool handler's return value is what tells the runtime whether the operation
succeeded. When that value is a plain dict, nothing in it says so. The runtime
has to guess from key names, and every reader of the result has to guess the
same way.

That guess cannot be finished. A dict carrying a `status` key may be declaring
a failure or merely reporting progress, and no set of key names separates the
two, because the value carries no type saying which it is. A handler is equally
free to return `{failed: true}` or `{error_code: 7}`, which no convention
covers, and those read as success.

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
    schema: "harn.agent_tool_handler_result.v1",
    text: "3 matches",
    data: {matches: 3},
  }
}
```

## Severity

This reports as a warning while in-tree handlers migrate. It becomes an error
once no untyped handler result remains, at which point outcome classification
stops being a heuristic over key names.
