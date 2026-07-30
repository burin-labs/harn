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

The legacy effectful globals are removed. This lint supplies an actionable
migration repair before the checker reports the removed symbol.

## How to fix

- Run `harn fix --apply --safety surface-changing` over the file. Calls inside
  an existing Harness boundary are rewritten in place; otherwise the fixer
  threads an explicit Harness parameter through local callers.
- Run lint again. `capability-attenuation` suggests replacing an unnecessarily
  broad helper parameter with `HarnessFs`.
