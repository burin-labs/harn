Functions, tools, pipelines, and `fn` closures can now declare a typed
exception channel with a `throws E` (or `throws (E1 | E2)`) clause after the
return type — e.g. `fn parse(s: string) -> Doc throws ParseError`. The clause is
optional and additive: a callable with no `throws` clause keeps today's
unconstrained behavior, so no existing code needs to change. A `throw` whose
value's type is not covered by the enclosing callable's declared `throws` set is
a type error, and a typed `catch` that fails to cover every declared thrown
variant is flagged.
