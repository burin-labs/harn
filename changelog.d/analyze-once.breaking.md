- `harn_lint::LintOptions` has a new public field, `binding_types`, so a
  struct literal that names every field no longer compiles. In
  `harn_parser::analysis`, `ParseOutput` and `TypeCheckOutput` now hold their
  source, program, and type facts as `Arc`s shared with the cache.

  Migration: end a `LintOptions` literal with `..Default::default()`, or pass
  the type facts you already have; read shared outputs through `Deref`, and
  copy one only to own or extend it.

  ```rust
  // Before
  let options = LintOptions { file_path: Some(path), /* every field */ };
  let mut diagnostics = output.diagnostics;
  diagnostics.extend(extra);

  // After
  let options = LintOptions {
      file_path: Some(path),
      binding_types: Some(&output.binding_types),
      ..Default::default()
  };
  let mut diagnostics = output.diagnostics.to_vec();
  diagnostics.extend(extra);
  ```
