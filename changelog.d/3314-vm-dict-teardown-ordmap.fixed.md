- **`harn-vm` compiles again after the persistent-`imbl::OrdMap` dict migration.**
  The iterative value-teardown worklist (`value/recursion.rs::dismantle_values`,
  added to bound native-stack depth when dropping deeply nested values) called
  `into_values()` on an owned `DictMap` — but `DictMap` is now an
  `imbl::OrdMap`, which (unlike `BTreeMap`/`HashMap`) has no `into_values`.
  This was a hard `error[E0599]` that broke every Rust compile of `harn-vm`
  (the `Audit scripts`, `Harn conformance + audit`, and crate-packaging release
  gates all failed on it). Fixed by moving owned values out via the map's
  owning `IntoIterator` (`into_iter().map(|(_, v)| v)`), preserving the iterative
  teardown semantics.
