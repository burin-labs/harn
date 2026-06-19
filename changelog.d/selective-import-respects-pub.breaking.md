- **Selective imports now respect `pub` visibility.** `import { name } from "m"`
  could previously bind a non-`pub` function of `m`, even though a wildcard
  `import "m"` would not see it — selective imports silently bypassed
  visibility. Now both forms expose the same surface: a module that marks any
  function `pub` exports only its `pub` functions (and `pub import`
  re-exports); a module that marks nothing `pub` still exports everything
  (the zero-ceremony fallback is unchanged). Importing a non-`pub` name from a
  module that has opted into explicit exports is rejected at `harn check` time
  with `HARN-IMP-002` (pointing at the import, suggesting `pub`) and at load
  time. **Migration:** mark the symbol `pub` if it is meant to be importable;
  to test a private helper, co-locate the test in the same file (it sees
  module-private functions directly). This matches TypeScript, Rust, and Go.
