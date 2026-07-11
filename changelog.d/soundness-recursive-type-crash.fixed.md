- **Recursive type aliases used as annotations no longer crash the compiler.**
  A self-referential type such as `type Tree = {value: int, children: [Tree]}`
  used as a parameter or binding annotation previously overflowed the stack and
  aborted `harn check` / `harn run` (SIGABRT). The subtype checker now closes the
  recursion coinductively (reflexive short-circuit plus a guard on the
  pre-resolution `(expected, actual)` pair, matching how the parser's
  `resolve_alias` already guards), and the compiler's alias expansion carries the
  same cycle cutoff. Recursive shapes are now usable as type annotations.
  (Known follow-up: a *recursive function* that traverses a recursive-typed
  parameter can still hang at runtime — tracked in #4451.)
