- **Scratchpad recitation is removed.** The opt-in `scratchpad` agent option, the `std/agent/scratchpad` module, the
  session scratchpad (`harness.agent.scratchpad`, `set_scratchpad`, `clear_scratchpad`, the `scratchpad` and
  `scratchpad_version` snapshot fields, the `agent_scratchpad` transcript events and `metadata.agent_scratchpad`) and
  the `agent_scratchpad_reorganization` agent event are gone. The `agent_loop` no longer adds a scratchpad as a
  reachability-GC root or write barrier.

  Migration: drop `scratchpad` from agent options and stop calling the three session scratchpad methods. To keep
  notes across turns, put them in the system prompt or a durable store such as `agent_state_*` or the memory
  helpers. A reachability-GC projection that needs a write barrier now takes `write_barrier_refs` explicitly. The
  `scratchpad` key of `transcript_project` stays as caller-supplied root material.
