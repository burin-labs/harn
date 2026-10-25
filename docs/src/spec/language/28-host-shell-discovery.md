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
  current host/session. Harn only chooses a shell whose command syntax its
  safety analyzer supports. An unrecognized login shell remains listed but
  can't become the implicit default.
- `process.set_default_shell` may be implemented by stateful hosts. Harn's
  standalone fallback stores a supported selection for the current
  thread/session and rejects an unsupported shell.
- `process.shell_invocation` resolves `{shell_id?, shell?, command?, login?,
  interactive?}` into `{program, args, command_arg_index, shell}`. When neither
  `shell_id` nor `shell` is supplied, it uses the selected default shell.

Shell-mode command runners may pass a shell object or shell ID resolved through
this capability, and otherwise use the selected default shell. `argv` mode
remains preferred for programmatic execution; shell mode is for user-authored
commands and interactive shell semantics. The normative schema is
`spec/schemas/host-shell-discovery.schema.json`.

Command-risk scans include `execution_semantics` with the request mode,
resolved dialect, resolution source, shell ID, and unresolved reason. An
explicit unsupported shell adds both `shell_dialect_unknown` and
`execution_semantics_unresolved`, so command policy still fails closed.

The same scan returns `shell_redirects`, projected from the command safety
parser. Each entry has `operator`, decoded `destination` (string or null),
`dynamic`, and `writes_file`. Quoted program text and literal argv arguments
are not shell operators. Actual redirects inside command substitutions and
literal nested-shell scripts remain in this projection. File-descriptor
duplication is omitted because it has no file destination; device destinations remain
visible for the consumer's policy. `shell_io_complete` is true only when
execution is resolved and both I/O projections cover its syntax. It is false
for Windows shell payloads, here-documents, here-strings, and prefixed nested
shell inputs whose I/O is not
fully projected. Consumers must not treat an incomplete or unresolved empty
list as evidence of no write.

`execution_stages` projects command argv from that same safety analysis, with
parallel `dynamic` flags for each resolved argument. `source_argv` preserves
the original arguments and assignment prefixes before executable unwrapping,
including standalone `env`. Redirect destinations are excluded
from argv; quoted program text remains an argument. Literal nested shell
payloads are expanded into their executed stages. Consumers must check the
same `shell_io_complete` flag before treating the I/O facts as complete.

For POSIX shell-mode requests, `shell_source_groups` preserves source chain
groups and pipeline stages before
nested-shell expansion. Each stage has decoded `source_argv` and unwrapped
`argv`. Assignment-only stages remain present with empty `argv`; outer
assignments on `sh -c` remain attached to that shell invocation. This lets
policy distinguish an executable's environment from literal argument text.
These are source facts, not expanded execution or environment values. Consumers
must check `execution_semantics` and handle dynamic or unsupported syntax
conservatively rather than interpreting an empty projection as a safe command.
The field is null for argv mode and other or unknown shell dialects.
