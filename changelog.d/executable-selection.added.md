- **Read executable selection without launching a process.**
  `harness.process.find_executable` uses the process launcher's existing search
  rules and returns a selected path or nil. Tool preparation can select an
  executable without running a version probe or writing a cache.
