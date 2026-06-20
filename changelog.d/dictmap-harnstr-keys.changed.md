- **Dict keys are now interned, refcounted strings instead of owned `String`s.**
  The map backing every `VmValue::Dict` changed from `OrdMap<String, VmValue>` to
  `OrdMap<HarnStr, VmValue>` (the thin one-word `arcstr::ArcStr` from the value
  shrink), and keys flow through a bounded interner (`harn_vm::value::intern_key`).
  Agent workloads are dict-heavy — the same field names (`role`, `content`,
  `arguments`, …) recur across thousands of message/JSON dicts — so each recurring
  key now shares a single allocation (a refcount bump) instead of allocating a
  fresh `String` per key, and dict tree nodes hold an 8-byte key instead of a
  24-byte one. The interner is bounded (keys up to 64 bytes, at most 8192 distinct
  entries) so high-cardinality or adversarial keys fall back to a plain allocation
  and can never grow it without bound. `VmValue::dict(...)` still accepts the
  `BTreeMap<String, _>` / `DictMap` maps callers already build (it interns on the
  way in). No `harn` language behavior changes.
