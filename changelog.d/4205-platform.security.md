- Harden scoped filesystem writes on Windows: the parent-directory walk now
  opens each component with `FILE_FLAG_OPEN_REPARSE_POINT` and refuses any
  junction or symlink reparse point mid-walk, closing the junction-traversal
  bypass that `O_NOFOLLOW` cannot cover on Windows.
- Add a recurrence-guard test that forbids raw path-based `create_dir_all` /
  `File::create` / `OpenOptions` inside the scoped-walk helpers and asserts
  every scoped leaf open keeps `O_NOFOLLOW`.
