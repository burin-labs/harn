- **Package git fetches no longer depend on the process working directory.** The
  hardened git runner spawned remote operations (`ls-remote`, `clone <url>
  <dest>`) without setting a working directory, so `git` inherited the process
  CWD and aborted with `fatal: Unable to read current working directory` if that
  directory had been removed — e.g. a `git`-backed dependency install running
  while another thread deleted the directory the process happened to sit in. The
  runner now takes an explicit `Cwd::In(path)` / `Cwd::Detached` choice, with
  remote operations detached to a neutral, guaranteed-to-exist directory;
  inheriting the process CWD is no longer expressible.
