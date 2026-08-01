Ambient legacy calls to privileged-wire builtins such as `security_policy`
lower to their `__name` runtime spellings, so agent-loop policy installation
no longer fails with an undefined ambient builtin.
