- Harden scoped filesystem writes on Windows: the parent-directory walk now
  opens each component with `FILE_FLAG_OPEN_REPARSE_POINT` and refuses any
  junction or symlink reparse point mid-walk, substantially narrowing the
  junction-traversal bypass that `O_NOFOLLOW` cannot cover on Windows. (A
  concurrent-swap TOCTOU window remains on Windows pending a handle-relative
  walk; the unix fd-walk is not affected.)
- Add a recurrence-guard test that forbids raw path-based `create_dir_all` /
  `File::create` / `OpenOptions` (and other path-resolving `std::fs`/`libc`
  calls) inside the scoped-walk and content-open helpers, and asserts every
  scoped leaf open keeps `O_NOFOLLOW`.
