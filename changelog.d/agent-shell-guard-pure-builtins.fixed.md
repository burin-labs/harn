`harn run --allow` and `harn watch --allow` no longer deny pure builtins such as `range` and
`lowercase`. They declare no effects, so denying them narrowed no authority and only made ordinary
calls fault one name at a time, as the standalone agent shell guard did on every command with a
`NAME=value` word.
