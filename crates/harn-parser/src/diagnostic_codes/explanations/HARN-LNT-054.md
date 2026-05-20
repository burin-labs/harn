# HARN-LNT-054 — ambient fs builtin replaced by `harness.fs.*`

**Category:** Lint (LNT)  
**Variant:** `Code::LintAmbientFsBuiltin` (ambient fs builtin)

## What it means

The lint fires on any call to `read_file`, `write_file`, `file_exists`,
`delete_file`, `append_file`, `list_dir`, `mkdir`, `copy_file`,
`temp_dir`, `stat`, `move_file`, `read_lines`, `walk_dir`, or `glob`.
These were ambient fs-capability builtins in the pre-`Harness` runtime.
Filesystem access now routes through the `harness.fs.*` sub-handle so
capability requirements appear in the type system instead of being
hidden in the stdlib surface.

This is a lint, not a hard error. The legacy builtins still compile
while the migration is in flight, but every new call site should use the
`harness.fs.*` method that matches it (`read_file` →
`harness.fs.read_text`, `write_file` → `harness.fs.write_text`, etc.).

## How to fix

- Run `harn fix --apply --safety scope-local` over the file. The
  `bindings/thread-harness-fs` repair rewrites every call site where a
  `harness` (or `_harness`) binding is in scope.
- If `harness` isn't reachable from the call site, first thread it
  through the enclosing fn via the `bindings/thread-harness` repair
  (which adds the `harness: Harness` parameter at the entrypoint), then
  re-run `harn fix --apply` to swap the call.

## Stability

This code is stable. Its identifier, category, and meaning will not
change without a deprecation cycle. Cross-language tooling and IDE
integrations can dispatch on it directly.
