A `for_command` grant now reaches a shell-mode command that is exactly one plain command naming that
program: Harn spawns the command directly, without the shell, so `gh issue list` in shell mode
receives a `for_command: "gh"` grant and nothing else does. Compound commands keep the shell and get
no command-bound grant. A `gh` grant reaches only the `gh` file resolved from trusted `PATH` entries
outside every workspace and write root, through a foreground `run_command`. The granted `gh` gets a
fresh owner-only config and extension directory outside those roots, an empty pager, a refusing
browser and editor, a trusted `PATH`, and git hooks, fsmonitor and `ext::` remotes turned off.
`gh auth token` and the other `gh auth` subcommands that print the credential are refused, and every
granted value, including its base64 and percent-encoded forms, is redacted from command output,
events and transcripts. A `secret_store` grant now resolves through the host's secret provider on
blocking tool calls instead of failing as unavailable. The confined-command `GH_CONFIG_DIR` stand-in
drops `!` aliases and the pager, browser, editor and socket keys. `gh` read subcommands (`list`,
`view`, `status`, `diff`, `checks`, and GET `gh api`) now count as workspace reads for tool-batch
phase planning; writes and unknown subcommands do not.
