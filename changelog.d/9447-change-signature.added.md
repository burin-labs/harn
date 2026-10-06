- **`code_index.change_signature` changes a function's parameters and rewrites
  every call site across files (#9447).** Pass `symbol_ref` and `params`, the
  complete new parameter list: an entry naming a current parameter keeps it
  (`from` renames it, and its body uses are renamed too), a new entry needs a
  `call_value` written at every call or a `default`, and a parameter left out
  is removed. Positional arguments are remapped by position, Python keyword
  arguments by name, and qualified, method, and macro-argument calls are
  included. The result is all-or-nothing and uses `rename_symbol`'s
  envelope, now built once in the refactor core for every code_index edit:
  `parameter_in_use`, `value_reference`, `unsupported_call_site`,
  `overrides_present`, `ambiguous_symbol`, and `no_match` write nothing and
  list the blocking `sites`, and a request that does not fit the function
  raises. Rust, TypeScript/TSX, and Python report `change_signature: true` in
  `ast.capabilities`. Calls inside Python f-strings and TypeScript template
  literals are now reference sites for every code_index refactoring, so
  `rename_symbol` renames them too.
