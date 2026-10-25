Local builds compose the workspace compiler wrapper before sccache, preserving
parallel compilation and compiler-object caching with both wrappers enabled.
Run `make setup-rust` to refresh an existing checkout's installed wrapper.
