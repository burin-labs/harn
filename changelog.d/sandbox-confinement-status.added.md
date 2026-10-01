- `harness.system.sandbox_confinement()` reports, before any spawn, whether
  this host can confine child processes, and the exact value an `os_hardened`
  spawn is refused with when it cannot (for example, Linux without Landlock).
  Embedders read it to warn once at startup instead of on every refused
  command.
