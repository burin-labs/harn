- **`process.exec` no longer clears the child environment by default when `env`
  is supplied.** Previously, passing an `env` dict without an explicit
  `env_mode` defaulted to `env_mode: "replace"`, which called `env_clear()` and
  silently dropped PATH/HOME/etc. — so `env: {ONE_VAR: "x"}` wiped the rest of
  the environment. The default is now `env_mode: "merge"`: the provided keys are
  overlaid on the inherited parent environment. Full replacement is still
  available by passing `env_mode: "replace"` explicitly. An unrecognized
  `env_mode` is now rejected instead of being treated as a non-replace mode. No
  in-tree caller relied on the clear-by-default behavior (the `std/git` path
  passes neither `env` nor `env_mode`, and the agent `run_command` tool uses the
  separate hostlib `inherit_clean`/`replace`/`patch` vocabulary, which is
  unchanged).
