- **`std/edit`'s parameter refactorings run on the code index (#9447).**
  `edit_change_signature`, `edit_add_parameter`, and `edit_reorder_parameters`
  now take `harness.code_index` instead of `fs`/`random`/`ast`, address the
  function by `symbol_ref`, and return `code_index.change_signature`'s result.
  `callsite_strategy`, `fill`, and the parameter-text arguments are gone, and
  the old single-file rewrite no longer exists.

  Migration: rebuild the code index, then pass structured parameters.

  ```harn,ignore
  // Before
  edit_add_parameter(harness.fs, harness.random, harness.ast,
    {path: "src/lib.rs", symbol: {name: "scale"}, param: "offset: i64", default: "0"})
  // After
  harness.code_index.rebuild({root: "."})
  edit_add_parameter(harness.code_index,
    {symbol_ref: {name: "scale", path: "src/lib.rs"},
     param: {name: "offset", type: "i64", call_value: "0"}})
  ```
