Restore `env_remove` support for `hostlib_tools_run_command` so stdlib command
wrappers and host integrations can strip caller-selected child environment
variables while keeping the rest of the inherited process environment intact.
