A background `sub_agent_run` whose policy sets `recursion_limit: 0` now runs its
own loop and is refused only when it tries to delegate further. The worker
carrier no longer applies the child's requested limit before the child's own
loop entry consumes the descent, which had refused every such child with
`nested execution budget exhausted before sub_agent_run`.
