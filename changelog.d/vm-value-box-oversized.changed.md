- **VM value size.** `VmValue` is now 24 bytes (down from 32). The two
  oversized inline payloads — `Range` (a `start`/`end`/`inclusive` triple) and
  `BuiltinRefId` (an id plus an `Arc<str>` name) — are boxed behind a shared
  pointer, so no variant inflates the common `Int` / `Float` / `List` / `Dict` /
  `String` shapes the interpreter copies on every push, pop, clone, and
  local-slot write.
