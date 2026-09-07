`process.run`, `exec_opts`, and `exec_at_opts` now start the child in a Windows verbatim-prefixed working
directory (the form `cwd()` and `canonicalize` return) instead of failing with os error 267, matching `process.exec`.
