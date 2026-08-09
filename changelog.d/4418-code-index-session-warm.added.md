Embedders can retain the shared `CodeIndexCapability` from
`install_default_with_handles` and call `warm_session` to restore a snapshot or
start a single-flight background rebuild without blocking the first agent turn.
Sync `hostlib_code_index_rebuild` joins the same gate so `ensure_initialised`
does not start a second full walk while the warm is still running.
