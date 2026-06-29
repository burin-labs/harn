- **`agent_fanout` no longer loses a whole batch when one child fails to spawn.**
  A background `sub_agent_run` validates and builds its request synchronously
  (e.g. parsing `allowed_tools`) and can also hit a host worker-spawn fault, so
  it can `throw` before any handle exists. The per-wave spawn loop had no
  per-child guard, so a single malformed unit aborted the entire wave *and*
  every later wave — silently dropping every sibling result. Each spawn is now
  caught individually: a spawn-time throw becomes that unit's own `ok: false`
  result (`status: "failed"`, the fault in `error`) at its correct offset, the
  surviving children are still joined, and results stay 1:1 and positionally
  aligned with `requests`. One bad unit can no longer nuke a parallel fan-out.
