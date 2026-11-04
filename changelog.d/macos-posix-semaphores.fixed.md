The macOS process sandbox now allows POSIX named semaphores, so Python's `multiprocessing.Lock()`
and the linters and test runners built on process pools no longer fail with `PermissionError: [Errno 1]`.
The network stays closed.
