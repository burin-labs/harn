A `for_command` grant now reaches a shell-mode command that is exactly one plain command naming that
program: Harn spawns the command directly, without the shell, so `gh issue list` in shell mode
receives a `for_command: "gh"` grant and nothing else does. Compound commands keep the shell and get
no command-bound grant. A `gh` grant reaches only `gh` reads (`list`, `view`, `status`, `diff`,
`checks`, and GET `gh api`), after skipping `-R`, `--repo` and `--hostname`; checkouts, clones,
forks, writes, aliases, extensions and unrecognized subcommands run without it. It reaches only the
native `gh` executable resolved from trusted `PATH` entries outside every workspace and write root,
through a foreground `run_command`, and never a script or version-manager shim. The granted `gh`
gets a fresh owner-only config and extension directory outside those roots, an empty pager, a
refusing browser and editor, a trusted `PATH`, and git hooks, fsmonitor and `ext::` remotes turned
off. The token is never in gh's environment: it is stored as a github.com login in the per-spawn
`hosts.yml`, which every other confined child is denied, so gh sends it to no other host. A granted
`gh` keeps only `LANG`, `LC_*`, `TERM`, `TMPDIR`, `TZ` and `NO_COLOR` from its caller; proxy, TLS,
debug and token variables are dropped. A granted read is refused when it names `--jq`, `-q`,
`--template` or `-t`, passes arguments after `--`, or names a host other than github.com, and when
a write root overlaps the user's git, SSH or gh configuration.
`gh auth token` and the other `gh auth` subcommands that print the credential are refused. Every
granted value, for any program, including its base64 and percent-encoded forms, is redacted from
command output, events and transcripts. A `secret_store` grant now resolves through the host's
secret provider on blocking tool calls instead of failing as unavailable. The confined-command
`GH_CONFIG_DIR` stand-in drops `!` aliases and the pager, browser, editor and socket keys. `gh` read
subcommands now count as workspace reads for tool-batch phase planning, including after a global
`-R`; writes and unknown subcommands do not.
