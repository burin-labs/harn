Functions, tools, pipelines, and `fn` closures can now declare a typed
exception channel with a `throws E` (or `throws (E1 | E2)`) clause after the
return type — e.g. `fn parse(s: string) -> Doc throws ParseError`. The clause is
optional and additive: a callable with no `throws` clause keeps today's
unconstrained behavior, so no existing code needs to change. A `throw` whose
value's type is not covered by the enclosing callable's declared `throws` set is
a type error (`HARN-TYP-026`). The check is catch-exhaustive: an error handled by
a `try`/`catch` is subtracted from the callable's thrown set (a typed `catch (e:
E)` handles enum errors of type `E`, an untyped `catch` handles everything), while
errors raised in a `catch`/`finally` body still escape — so a `try`/`catch` that
leaves an error uncovered must declare (or handle) it.
