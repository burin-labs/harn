- Compiler front end is substantially faster: the lexer scans bytes and
  slices token text instead of walking a `Vec<char>` (~1.5x tokenize
  throughput), and the type checker shares scope tables structurally
  instead of deep-cloning them on every block entry (~1.8x whole-file
  typecheck), with the legacy-capability env flag cached and builtin
  signature fallback lookups indexed. The shared AST visitor no longer
  allocates per visited node, which also speeds the kernel compiler's
  module-context and closure-capture passes plus every visit-based lint.
  Cold-start compiles, `harn check`, `harn fmt`/`harn lint`, and the LSP
  all inherit the win.
- VM call and dispatch hot paths shed fixed overhead: entering a user
  function no longer clones the function-name `String` (per-call
  allocations drop from 3 to 2), steady-state inline-cache hits on
  arithmetic/comparison and direct calls no longer rewrite the cache
  entry, string-constant pushes are lock-free, dict `for`-loops stop
  re-allocating keys and re-interning literal entry keys per iteration,
  and one-part string interpolation re-shares the existing string.
