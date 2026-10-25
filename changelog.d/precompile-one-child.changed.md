`harn precompile <dir>` now compiles every source in one child process on a
worker pool instead of starting a process per source. The child shares process
startup, stdlib preparation, and each shared import's parse across the whole
tree. Output, artifacts, the summary, and the exit status are unchanged.
