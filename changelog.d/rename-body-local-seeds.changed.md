- `code_index.rename_symbol` on the syntax-tree backend now renames a symbol
  declared inside a body: a TypeScript or JavaScript `const`, `let`, class,
  type, interface or enum in a block, a function declared directly in a
  function body, a Rust item in a block or a private item of an inline module,
  and a Python def or class in a function body. Only that scope is rewritten,
  so same-named declarations elsewhere no longer force `ambiguous_symbol`. Any
  spelling in the scope that the proof cannot bind, such as a shadowing
  parameter, a shorthand property or a binding macro, still refuses the whole
  rename without writes. TypeScript `typeof name` in a type is now a proven
  reference.
