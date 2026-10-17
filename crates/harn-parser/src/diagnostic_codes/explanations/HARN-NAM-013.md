# HARN-NAM-013 — method spelling was removed from the builtin receiver

Raised when `receiver.method()` uses a spelling that builtin values no longer
accept. Each operation on a string, list, set, dict, or range has one name:
`lower`/`upper` instead of `lowercase`/`to_lower`, `len` instead of `count`,
`contains` instead of `includes`, `windows`/`chunks` instead of
`window`/`each_cons`/`chunk`, `map_keys` instead of `rekeyed`, `merge` instead
of `merging`, and `xs.push(v)` instead of `xs = xs.appending(v)`.

## How to fix

- Run `harn fix --apply --safety behavior-preserving` to rewrite renamed
  spellings, and `xs = xs.appending(v)` to `xs.push(v)` (other `appending`
  uses become `xs + [v]`).
- For a spelling with no mechanical equivalent, follow the message:
  `s.adding(v)` becomes `s.add(v)` on a `let` binding or
  `s.union(set([v]))` for a new set, and `d.removing(k)` becomes
  `d.remove(k)` on a `let` binding or `omit(d, [k])` for a new dict.
