# HARN-TYP-026 — thrown value is not covered by the declared `throws` set

## What it means

A function, tool, pipeline, or `fn` closure declared a typed exception channel
with a `throws E` (or `throws E1 | E2`) clause, but the callable can surface a
thrown value whose type is not a member of `E`. Harn checks every `throw` site —
and every value propagated out of a `try`/`catch` — against the declared set, so
the declared channel is an exhaustive description of what the callable may throw.

The clause is opt-in: a callable with no `throws` clause is not throw-checked and
never raises this error. Once a clause is present, though, it must account for
every escaping error.

## Catch-exhaustiveness

An error handled inside the callable does not count against its `throws` set. The
check mirrors the runtime:

- A typed `catch (e: E)` handles a thrown error only when its type is `E` (the VM
  matches thrown *enum* errors by name and rethrows the rest), so it subtracts
  `E` from what escapes.
- An untyped `catch` is a catch-all and absorbs every error the body can throw.
- A `throw` in a `catch` or `finally` body always escapes and is checked against
  the declared set.

So a `try`/`catch` whose handler does not cover an error the body can throw makes
that error part of the callable's thrown set — and it must then be declared (or
handled) or this error fires.

## How to fix

- Add the missing type to the `throws` clause, e.g. widen `throws NotFound` to
  `throws NotFound | ParseError`.
- Handle the error inside the callable with a `catch` that covers its type, so it
  no longer escapes.
- Convert the thrown value to a declared type before it leaves the callable.
- Remove the `throws` clause entirely if the callable should not constrain what
  it throws (this reverts to the historical unconstrained behavior).
