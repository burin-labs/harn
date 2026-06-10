- **`exec_opts` / `exec_at_opts` — an options form for the convenience exec
  builtins.** `.harn` callers that need to pass `env`, `cwd`, or a `timeout` no
  longer have to drop to the verbose `host_call("process.exec", {mode: "argv",
  argv: [...], env, env_mode})`. The new builtins take an argv list plus an
  options dict: `exec_opts(["git", "clone", a, b], {env: {...}, timeout: 30000})`
  and `exec_at_opts(dir, ["git", ...], {env_mode: "replace"})`. Options are
  `{env?, env_mode?, cwd?, timeout?}` (`timeout`/`timeout_ms` in milliseconds),
  and the result is the same `{stdout, stderr, status, success}` shape as `exec`
  plus `timed_out`/`duration_ms`. The positional `exec("ls", "-la")` /
  `exec_at(dir, ...)` forms are unchanged.
