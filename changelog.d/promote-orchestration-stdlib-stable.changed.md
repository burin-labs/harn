- Promote the settled re-architecture orchestration stdlib surfaces from
  `@api_stability: experimental` to `stable`: `std/agent/{pins,goal,lanes,overlays,host_tools}`
  (including `agent_edit_tools`) and `std/workflow/repair`. The governor / stall /
  judge / agent-loop-options surfaces stay experimental while #3943 (editless-stop
  eval convergence) is in flight.
