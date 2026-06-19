<!-- Generated from spec/chapters/*.md by scripts/sync_language_spec.harn -->

## Host shell discovery

The `process` host capability owns shell discovery and shell-mode invocation.
This keeps IDEs, TUIs, headless CLI runs, and cloud workers on the same
contract instead of hardcoding `/bin/sh`, `cmd`, or host-specific settings in
each integration.

- `process.list_shells` returns `{shells, default_shell_id}`. Each shell entry
  has `id`, `label`, `path`, `platform`, `available`, `supports_login`,
  `supports_interactive`, `default_args`, `login_args`, and `source`.
- `process.get_default_shell` returns the selected shell object for the
  current host/session.
- `process.set_default_shell` may be implemented by stateful hosts. Harn's
  standalone fallback stores the selection for the current thread/session.
- `process.shell_invocation` resolves `{shell_id?, shell?, command?, login?,
  interactive?}` into `{program, args, command_arg_index, shell}`. When neither
  `shell_id` nor `shell` is supplied, it uses the selected default shell.

Shell-mode command runners may pass a shell object or shell ID resolved through
this capability, and otherwise use the selected default shell. `argv` mode
remains preferred for programmatic execution; shell mode is for user-authored
commands and interactive shell semantics. The normative schema is
`spec/schemas/host-shell-discovery.schema.json`.

