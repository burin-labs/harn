# HARN-LNT-054 — ambient fs builtin replaced by `harness.fs.*`

## What it means

The lint fires on any call to `read_file`, `write_file`, `file_exists`,
`delete_file`, `append_file`, `append_file_locked`, `list_dir`, `mkdir`,
`copy_file`, `temp_dir`, `mkdtemp`, `stat`, `move_file`, `read_lines`,
`walk_dir`, `glob`, or `find_text`.
These were ambient fs-capability builtins in the pre-`Harness` runtime.
Filesystem access now routes through the `harness.fs.*` sub-handle so
capability requirements appear in the type system instead of being
hidden in the stdlib surface.

This is a lint, not a hard error. The legacy builtins still compile
while the migration is in flight, but every new call site should use the
`harness.fs.*` method that matches it (`read_file` →
`harness.fs.read_text`, `write_file` → `harness.fs.write_text`, etc.).

## How to fix

- Run `harn fix --apply --safety scope-local` over the file. By default the
  fixer rewrites ambient filesystem calls to the VM-level `harness` binding
  with `bindings/use-enclosing-harness-global`, preserving helper signatures.
- If you explicitly want source-level parameter threading, run
  `harn fix --apply --safety surface-changing --harness-threading thread-params`.
  `harn fix --plan --json` reports which signatures would change and whether
  cross-module callers must be updated.
