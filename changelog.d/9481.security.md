- The code-index refactorings (`rename_symbol`, `change_signature`, `extract_function`) now pass every
  file they read or write through one containment check. A path must resolve inside the indexed
  workspace after symlinks resolve, and inside the sandbox's `workspace_roots` when a restricted
  profile is active. Before this, a directory replaced by a symlink after indexing sent
  `rename_symbol` and `change_signature` writes outside the workspace. `rename_symbol` also ignored
  a sandbox scope narrower than the index, and `extract_function` without an index wrote outside the
  sandbox scope. An escaping file now refuses the whole operation with a sandbox violation before
  anything is written.
