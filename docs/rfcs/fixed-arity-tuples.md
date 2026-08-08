# Fixed-arity tuple types

Status: accepted and implemented

Tracking issue: [#4553](https://github.com/burin-labs/harn/issues/4553)

## Decision

Harn has a structural `tuple<T0, T1, ...>` type as a fixed-arity refinement of
its existing value-semantic list runtime. The language has one positional
collection representation:

- `tuple(a, b)` explicitly infers `tuple<A, B>`.
- A bracket literal adopts a tuple type when a `tuple<...>` annotation or
  function parameter supplies that expected type.
- An unannotated bracket literal continues to infer `list<T>`, including
  heterogeneous and `const` literals.
- A spread in `tuple(...)` makes arity unknown and therefore infers a list.

This is a static and runtime-boundary contract, not a second collection value.
The VM continues to store both lists and tuples as its list value. A tuple
parameter guard additionally validates exact length and every positional type.

## Why this seam

Before this decision, Harn collapsed every bracket literal to a homogeneous
element union. Even a known two-element value such as `[1, "one"]` had type
`list<int | string>`, so its index `0` read as `int | string | nil`. The
runtime already knew the literal's length and position, but the type system
discarded both facts.

Inferring every bracket literal as a tuple would recover those facts at the
cost of changing the natural type of existing list code. Harn has many
incremental builders starting at `[]`, and list APIs intentionally accept
ordinary bracket literals. Heterogeneity and `const` are also insufficient
signals: heterogeneous lists are supported, and `const` controls binding
replacement rather than selecting a collection kind.

Comparable language designs reinforce the explicit/contextual boundary:

- [TypeScript tuples](https://www.typescriptlang.org/docs/handbook/2/objects.html#tuple-types)
  use a distinct tuple type, while
  [`as const`](https://www.typescriptlang.org/docs/handbook/release-notes/typescript-3-4.html#const-assertions)
  is an explicit request for readonly literal precision.
- [Python's type system](https://docs.python.org/3/library/typing.html#annotating-tuples)
  distinguishes fixed heterogeneous `tuple[T0, T1]` from variable-length
  `tuple[T, ...]`.
- [Dart records](https://dart.dev/language/records) and
  [Swift tuples](https://docs.swift.org/swift-book/documentation/the-swift-programming-language/types/#Tuple-Type)
  give product values distinct syntax instead of reinterpreting arrays.
- [C# collection expressions](https://learn.microsoft.com/en-us/dotnet/csharp/language-reference/proposals/csharp-12.0/collection-expressions)
  are target typed and deliberately have no natural fixed collection type.

Harn combines explicit construction with contextual literals because this
retains one runtime collection and keeps typed call sites concise.

## Static contract

### Indexing and iteration

- Constant non-negative and negative in-bounds indexes select one position.
- Constant out-of-bounds reads produce `HARN-TYP-027`.
- Dynamic reads produce the union of the positional types plus `nil`.
- Iteration and a list containing tuples preserve each nested tuple type; direct
  tuple iteration yields the positional union without `nil`.
- Destructuring binds each known position independently. A rest binding is a
  list of the remaining positional union.

### Subtyping and widening

Tuple-to-tuple subtyping requires equal arity and is covariant position by
position. `tuple<A, B>` satisfies `list<T>` exactly when both `A` and `B`
satisfy `T`. The inverse is rejected because a list does not prove arity.

Arity-changing operations intentionally forget positional facts. Slices,
`appending`, and collection transforms return lists whose element type is the
union of reachable tuple positions. A spread passed to `tuple(...)` likewise
returns a list, though fully dynamic spread sources may leave its element type
gradual. This is the single widening seam; there is no mutation-time evolution
of a tuple's declared positions.

### Writes and value semantics

Harn has no shared mutable collection aliases: assignment and argument passing
copy collection values, and methods return new values. Tuple covariance is
therefore sound for the same reason list covariance is sound.

A constant-index write is checked against that slot. A dynamic write must be
valid for every slot it could select. Runtime out-of-bounds write behavior is
unchanged.

## Runtime and compatibility

No new serialized value tag, equality rule, display format, host bridge shape,
or persistence format is introduced. This keeps tuples compatible with JSON
arrays, existing list builtins, transcripts, and host adapters. Runtime type
guards distinguish tuple annotations structurally by exact arity and
position.

The feature is additive and opt-in, so it does not need a rollout flag.
Existing bracket-literal inference, list indexing, runtime values, and APIs are
unchanged.

## Rejected alternatives

- **Tuple-by-default bracket literals:** breaks existing list inference and
  builder patterns.
- **Heterogeneous-only inference:** makes adding or removing a union member
  silently change collection kind and does not help homogeneous fixed pairs.
- **`const`-only inference:** conflates binding replacement with data shape.
- **A second VM tuple value:** duplicates collection behavior and adds bridge,
  serialization, equality, and persistence seams without a product benefit.
- **Special-casing literal subscripts:** recovers one expression but loses the
  contract through variables, parameters, loops, destructuring, and runtime
  boundaries.

## Non-goals

This decision does not add labeled positions, optional/rest tuple positions,
tuple-specific equality, a new pattern syntax, or positional-item schema
materialization. The last requires an extension to Harn's canonical schema
vocabulary; runtime parameter guards use the exact tuple contract directly in
the meantime. These features require independent use cases and should extend
the tuple contract rather than creating another positional type system.
