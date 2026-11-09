A background `sub_agent_run` now runs its own loop under its parent's policy and
applies its own `policy` the way any `agent_loop` does: the recursion cap at loop
entry and the tool and capability ceilings at each tool dispatch. The worker
carrier used to fold the child's policy in first, so a leaf child
(`recursion_limit: 0`) was refused with `nested execution budget exhausted
before sub_agent_run`, and a tool-scoped capability ceiling refused the loop's
own runtime calls before its first model call.
