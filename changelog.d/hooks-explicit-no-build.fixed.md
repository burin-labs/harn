Pre-push checks honor `HARN_BIN_NO_BUILD=1`: they refuse when no proven worktree runtime is available,
without compiling, bootstrapping a compiler wrapper, or signing a replacement.
Proven runtimes still run the required reader census.
