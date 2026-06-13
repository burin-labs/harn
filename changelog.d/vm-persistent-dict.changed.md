- **VM dicts are now persistent.** `VmValue::Dict` is backed by a structurally
  shared `imbl::OrdMap` instead of a `BTreeMap`. Copy-on-write dict mutation —
  performed on every `dict[key] = value` / property assignment when the value is
  aliased (on the stack, in another local, or captured by a closure) — drops
  from an O(n) deep clone of every key and entry to an O(log n) path copy,
  removing the dominant allocation cost in mutation-heavy scripts. Dict
  ordering, equality, identity (`===`), iteration, and the full read/write API
  are unchanged.
