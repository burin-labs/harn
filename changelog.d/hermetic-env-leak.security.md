A hermetic session profile no longer leaks the parent environment into spawned
child processes. The profile's contract is that no credential crosses into a
child, but most spawn seams built their `Command` without consulting
`security::resolve_env`, so children inherited every variable the session held —
credentials included. Affected seams included the `process.exec` / `process.spawn`
host ops, `spawn_captured`, `exec_opts` / `exec_at_opts`, the workflow `verify`
node, the eval-pack runner, the LLM routing verifier, and — most significantly —
the spawner behind the agent's own `run_command` tool, which scrubbed secrets
with a name-pattern denylist rather than the profile's allowlist.

The environment is now closed at the sandbox funnel (`std_command_for`,
`tokio_command_for`, `command_output`) that every one of those seams already
routes through, so a new spawn site cannot silently opt out of the contract.
Callers still layer their own `env` / `env_remove` on top. Sessions with no
profile — the default, and currently every session — are unchanged.
