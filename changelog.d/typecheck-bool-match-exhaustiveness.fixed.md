- **Exhaustive `match` on a `bool` no longer reports a false "can fall
  through" error.** A `match` over a `bool` scrutinee that covers both `true`
  and `false` with returning arms is now recognized as exhaustive and
  terminating, matching how Rust and Swift treat a `match`/`switch` over a
  boolean — no wildcard arm required.
