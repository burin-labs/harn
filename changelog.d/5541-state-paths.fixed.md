- Eight more conformance cases stopped writing into the checkout's `.harn`.
  They hardcoded relative paths — `path_join(".harn", "tmp", ...)` for scratch,
  or `.harn` as a state directory — which resolve against the runner's working
  directory. Because `.gitignore` ignores `.harn/` globally, this pollution was
  invisible rather than absent. The scratch cases now use
  `harness.fs.temp_dir()` directly, which is what `.harn/tmp` was standing in
  for.
