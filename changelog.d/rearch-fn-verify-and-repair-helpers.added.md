- Workflow stages gain three ergonomics on top of the retry-with-feedback
  machinery. A stage's `verify` may now be a **function** (fn-verify): it
  receives the settled attempt result and returns `{ok, findings?}` (or a
  bool), gates the retry branch on *any* stage kind, and threads its findings
  into the next attempt's repair prompt. `workflow_stages`
  (`std/workflow/patterns`) expands a concise linear `WorkflowStagesSpec` into
  the `{entry, nodes, edges}` graph `workflow_execute` consumes — pure sugar
  whose output is byte-identical to the hand-authored `workflow_graph`. And
  `workflow_run_repair` (new `std/workflow/repair`) runs the
  run→validate→repair loop as a first-class helper: one agent stage, a
  caller-supplied verifier (callable / `{command}` / `{assert_text}`), and
  automatic re-prompting with the findings up to `max_attempts`. All three
  reuse the embedded PR-I2 attempt loop (`std/workflow/stage.harn`); none adds
  a loop of its own.
