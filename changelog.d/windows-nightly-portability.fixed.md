- Write local dependency paths into `harn.toml` with POSIX separators so a
  manifest authored on Windows stays portable to Unix checkouts instead of
  embedding backslashes that fail to resolve.
- Return temp-directory builtin paths (`mkdtemp`, `mkdtemp_in_workspace`,
  `workspace_temp_dir`) in OS-normal form on Windows. A canonicalized workspace
  root carries a `\\?\` verbatim prefix in which `/` is a literal character, so
  the documented `mkdtemp_in_workspace(...) + "/child"` pattern previously failed
  with a path-not-found error.
- Retry the Windows atomic-replace step (`harn_vm::atomic_io`) on transient
  sharing-violation / access-denied errors so a virus scanner, indexer, or
  lagging handle close briefly holding a destination open no longer fails a
  durable write of transcripts, run records, snapshots, or other persistent
  state.
- Make `harn_vm::atomic_io` durable writes robust to long Windows paths. The
  temp sibling's name is now bounded so it can never be longer than the
  destination it replaces (its previous name embedded the full destination name,
  so a destination that fits under the 260-char `MAX_PATH` could still produce an
  overflowing temp path), and the atomic replace now applies the `\\?\`
  extended-length prefix to its `MoveFileExW` operands — matching what the
  standard library already does for its own file APIs — so a destination longer
  than `MAX_PATH` (for example a deeply nested filesystem snapshot body) no
  longer fails with a path-not-found error.
