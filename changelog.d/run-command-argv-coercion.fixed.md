`run_command` (the agent host `run` tool) now accepts the command vector under
either `argv` or `command`. Models frequently send the array under `command`
(e.g. `run({command: ["bash", "-lc", "ls"]})`) because the tool description says
"pass argv as an array of strings"; that previously threw
`run_command: argv must be a non-empty list of strings`. A list under `command`
is now coerced to argv mode (and works even when shell mode is disabled), and a
shell string passed while shell mode is disabled now returns an actionable error
instead of the misleading argv message.
