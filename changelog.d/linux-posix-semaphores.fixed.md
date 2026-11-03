The Linux process sandbox now lets confined commands use POSIX semaphores, so Python's
`multiprocessing.Lock()` and the linters and test runners built on process pools no longer fail
with `PermissionError: [Errno 13]`. Bubblewrap and namespaced launches get a private `/dev/shm`;
Landlock-only launches can read, write, and delete the same user's shared-memory segments there.
Shared values, arrays, and `shared_memory` still need truncation, which stays denied.
