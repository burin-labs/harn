Let a served turn in the default `ask` ACP mode run a pipeline's own bookkeeping. `harness.runtime.store_set` and
`store_delete` now carry `runtime_control_plane`, as `harness.agent.open` does, so the `read_only` ceiling that every
non-`code` session mode installs no longer rejects a pipeline's run-store writes; `store_save` and `store_clear` stay
capped. `harness.agent.goal` is also readable while a tool prepares its approval facts, so a tool's `prepare` hook can
bind the session goal.
