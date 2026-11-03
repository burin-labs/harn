A confined child now keeps a toolchain cache variable (`UV_CACHE_DIR`, `GOMODCACHE`, `XDG_CACHE_HOME`, and the rest of
the toolchain cache set) that the launcher pointed under one of the policy's `process_sandbox.write_roots`. Before, any
inherited cache path outside the workspace was replaced with an empty sandbox cache, so a warm package cache an embedder
had granted for writing was never used and an offline resolve failed in the child. A path outside both the workspace
and the write roots is still relocated.
