# ADR 0001: Pipe Operator With Explicit Placeholder

## Status

Accepted.

## Context

Harn already has method chaining for receiver-oriented collection and string
operations, but many stdlib helpers are free functions. Without a general
composition form, scripts either nest calls or allocate short closures whose
only job is to name the previous value.

Issue #921 asked us to choose between:

- `x |> f(_) |> g(_)`, an explicit pipe placeholder.
- `x.f().g()`, method-chain sugar that maps free functions taking `self` first
  into method calls.

## Survey

I surveyed representative scripts from `examples/` and `conformance/tests/`:

- `examples/data-transform.harn`: closure pipes over list filters/maps; wants
  lighter free-function and collection composition.
- `examples/stress-test.harn`: closure pipes over string transforms; reads as
  sequential data refinement.
- `examples/mcp-client.harn`: `exec(...).stdout.trim()`; receiver methods
  already work well.
- `examples/parallel-pipeline.harn`: collection `.filter(...)`; method chains
  are fine for receiver methods.
- `conformance/tests/collections/iter_laziness.harn`: long iterator method
  chains; iterator APIs should stay method-first.
- `conformance/tests/collections/iter_combinators_map_filter.harn`:
  `.filter().map().to_list()`; no need to rewrite fluent receiver APIs.
- `conformance/tests/collections/stream_operators_core.harn`: nested
  `stream.collect(stream.map(...))`; free functions benefit most from pipe.
- `conformance/tests/stdlib/json_query.harn`: free-function query helpers; free
  functions need composition without nesting.
- `conformance/tests/language/multiline_logical.harn`: multiline closure pipes;
  operator-leading continuation is already accepted.
- `conformance/tests/functions/tail_call_pipe.harn`: `return n - 1 |> process`;
  pipe should compose with function values and TCO.

The common pattern is mixed: receiver APIs should remain receiver APIs, while
free-function composition needs a separate simple surface. Method-chain sugar
would blur that boundary and require overload-style resolution for every free
function name. The explicit placeholder keeps the transformation local and
obvious.

## Decision

Use `|>` as the canonical composition operator. The left side is evaluated once
and passed into the right side:

- `value |> callable` calls `callable(value)`.
- `value |> f(_)` rewrites `_` occurrences in the right-hand expression to the
  piped value.
- `value |> _.method()` uses the placeholder as a normal expression receiver.

The placeholder rewrite does not descend into nested closure bodies, so a mapper
such as `items |> _.map({ x -> x + 1 })` does not accidentally capture the
outer pipe value inside the mapper.

## Consequences

- Existing receiver-style chains remain idiomatic: `iter(xs).map(...).to_list()`.
- Free-function chains avoid nesting: `text |> split(_, " ") |> len(_)`.
- Formatting can keep `|>` as a normal low-precedence binary operator with
  operator-leading multiline layout.
- Type checking can infer simple pipe results from closures, user functions,
  and statically typed builtins.
- Future lint rules should prefer pipe-placeholder form over closure wrappers
  when the closure only forwards its single argument into a free-function call.
