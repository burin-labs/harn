`change_signature` refuses with `fixed_signature` when a test, bench or entry
attribute, `fn main`, or a declared function type fixes the parameter list.
Rust `move_symbol` refuses with `visibility_required` when the source reads the
moved struct's private fields, refuses with `nested_import` when a nested import
list names the item, and keeps source imports the remaining code still uses. A
declaration that is not top level refuses with `not_top_level`. Symbols declared
in a function body, a callback, or an inline Rust module now carry a container.
`change_signature` accepts `symbol_ref.col` and selects as `rename_symbol` does.
