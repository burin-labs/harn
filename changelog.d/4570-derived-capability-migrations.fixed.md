`harn lint` and `harn fix` now find where a global went whenever it kept its
name on the handle it moved to. Recipes used to be a hand-written table, so a
capability that gained a method without someone also editing that table left
callers with a bare "not defined" and no repair. Across one large downstream
corpus that gap covered 105 of 169 removed globals.

The recipe is now read off the capability surface itself. `exit` resolves to
`harness.runtime.exit`, `hostlib_code_index_rebuild` to
`harness.code_index.rebuild`, and `agent_session_open` to `harness.agent.open`.
A name that several methods answer to is settled by parameter list, and a name
that is still a callable global stays uncovered, because it has not moved.

A new drift check fails the build when a global moves onto a handle without a
repair, so the next capability rename cannot quietly reopen the gap.
