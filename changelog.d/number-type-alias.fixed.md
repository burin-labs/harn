- **`number` is now a usable static type.** The runtime already accepted
  `number` as `int | float`, but the static type checker treated it as an opaque
  name, so `fn f(x: number) -> number { x + 1 }` raised spurious type errors at
  every use and arithmetic site. `number` now resolves to `int | float`
  everywhere (assignment, argument, return, and arithmetic), exactly like an
  explicit `int | float` annotation.
