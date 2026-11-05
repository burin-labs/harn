- **Removed `std/agent/completion_gate` and `std/agent/completion_behavior_evidence`.**
  `agent_completion_gate`, `agent_completion_gate_preview`, `agent_completion_gate_veto_counts`,
  `agent_completion_gate_record_veto`, `completion_gate_verdict`, `completion_gate_budget`,
  `completion_behavior_evidence`, `completion_behavior_run_check` and their option and fact types are gone, along
  with the `completion_gate` row accepted by `agent_preset_register` packs. No in-tree caller used the configured
  gate outside its own tests. Supply a `verify_completion` callback that returns
  `{confirm, message?, reason?, verification?, requirement_report?}`, and set `verify_completion_judge` or
  `turn_end_condition` for a bounded model judge. The judge arbitration, `completion_requirements` and
  `verify_completion` seams are unchanged.
