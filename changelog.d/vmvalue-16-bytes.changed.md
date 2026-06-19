- **The core `VmValue` runtime value is now 16 bytes (down from 24).** Every
  value the interpreter pushes, pops, clones, and writes to a local slot — plus
  every element of a `list` and every entry on the stack — is a third smaller,
  improving cache density across the whole VM. The shrink boxes the four
  oversized payloads (`Decimal`, `StructInstance`, and the `Range`/`BuiltinRefId`
  already-boxed cases) behind a shared pointer and replaces the 16-byte
  `Arc<str>` fat pointer behind the string-shaped variants (`String`,
  `BuiltinRef`, `TaskHandle`) with a one-word thin string (`arcstr::ArcStr`,
  re-exported as `harn_vm::value::HarnStr`). String-literal loads and enum/struct
  field reads stay zero-copy refcount bumps. No `harn` language behavior changes;
  this is an internal representation change (the unsafe pointer work lives in the
  vetted `arcstr` crate, not in Harn).
