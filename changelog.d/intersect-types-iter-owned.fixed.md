- **Typechecker: `intersect_types` now handles `iter<T>` and `owned<T>`.**
  Both kinds had no entry in the intersection table, so any `schema_is(x,
  S)` whose static `x` happened to be an `iter` or `owned` value silently
  dropped the relevant union member and left `x` un-narrowed in the
  truthy branch. `iter<T>` now intersects with `Named("iter")` and with
  another `iter<T>` the same way `list<T>` does. `owned<T>` is
  transparent at the equality boundary but the annotation survives the
  intersection — `owned<channel> ∩ channel = owned<channel>` — so the
  HARN-OWN-005 leak lint keeps tracking the narrowed binding.

- **Typechecker cleanup: collapse `Named ↔ parameterised` arms in
  `intersect_types`.** Each `(Named, T) / (T, Named)` pair produces the
  same intersection regardless of operand order, so the 12 individual
  arms (`Shape`, `DictType`, `List`, `Iter`, `Generator`, `Stream`, plus
  `unknown`/`any`) now share a single OR-pattern per kind. The width-
  subtyping merge for `Shape ∩ Shape` and the Union-distributes-over-
  intersection logic both move into named helpers (`intersect_shapes`,
  `intersect_union_with`) so the top-level match reads as a kind table
  without lossy duplication.
