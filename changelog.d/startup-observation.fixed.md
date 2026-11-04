- **Startup observations use the actual dispatch boundary.** Deterministic
  mocks and served calls publish the same inference start and completion
  events as live provider requests. A refused request publishes no dispatch.
  Host adapters can also observe the operating system's process birth on a
  monotonic clock, including work before the executable entry point, with
  the native clock resolution retained.
