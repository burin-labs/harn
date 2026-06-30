- When a restricted policy declares no explicit `workspace_roots`, the file
  write/read jail now falls back to the active session's workspace anchor and
  the host-declared `HARN_PROJECT_ROOT` project before the process execution
  cwd. Dispatched sub-agent workers running where the process cwd differs from
  the project (the eval pattern) can now write into the project instead of
  being rejected with a `HARN-CAP-201` sandbox violation rooted at the cwd.
