- `code_index.change_signature` (and `edit_change_signature`,
  `edit_add_parameter`, `edit_reorder_parameters`) now changes a trait,
  interface, or overridden method together with its whole override family:
  the root declaration, every implementation and override in the workspace,
  and every call. It covers Rust trait methods, TypeScript interface, abstract
  and class methods, and Python methods. It still refuses with
  `overrides_present` when a member implements a supertype outside the
  workspace, is written in a macro, a class field or a generated file, or
  belongs to a type no single supertype relates; `sites` names each blocker.
- A Rust macro body that declares `fn name(..)` is no longer rewritten as a
  call of `name`.
