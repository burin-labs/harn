# HARN-LNT-064 — mutable capture reassigned across a concurrency boundary

A variable declared in an enclosing scope is reassigned inside a `parallel` or
`spawn` body. Harn closures capture by reference, and `parallel`/`spawn` bodies
are lowered into closures whose captured cells are shared — by `Arc` — with
every concurrent branch. Reassigning such a variable therefore mutates a single
shared cell from many branches at once.

This is memory-safe (the cell is a mutex), but the writes race: branches
interleave at every `await` point (an `llm_call`, a host call), so a
read-modify-write such as `total = total + x` silently loses updates, and under
a multi-threaded runtime it is a genuine data race in the logical sense.

## How to fix

- Prefer returning a value from each branch and combining the results after the
  fan-out, which is deterministic and lock-free:

  ```harn
  const parts = parallel each items { item -> compute(item) }
  const total = fold(parts, 0, { acc, part -> acc + part })
  ```

- If a shared mutable accumulator is genuinely required, guard it explicitly and
  accept that ordering is nondeterministic.
- Reassigning a variable that is declared *inside* the branch body is fine and
  never flagged — only captures from an enclosing scope trip this lint.
