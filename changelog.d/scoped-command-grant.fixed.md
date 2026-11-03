A `for_command` grant now reaches a shell-mode command that is exactly one plain command naming that
program: Harn spawns the command directly, without the shell, so `gh issue list` in shell mode
receives a `for_command: "gh"` grant and nothing else does. Compound commands keep the shell and get
no command-bound grant. A granted `gh` runs with an empty pager, a refusing browser and editor, an
empty extension directory, and a `config.yml` stand-in without `!` shell aliases, so it cannot hand
the grant to another program. The confined-command `GH_CONFIG_DIR` stand-in drops `!` aliases too.
`gh` read subcommands (`list`, `view`, `status`, `diff`, `checks`, and GET `gh api`) now count as
workspace reads for tool-batch phase planning; writes and unknown subcommands do not.
