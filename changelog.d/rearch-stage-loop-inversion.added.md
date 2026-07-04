- Workflow stages support retry-with-feedback. `retry_policy.feedback`
  (`true` or `{max_chars}`) appends the prior attempt's verification findings
  to the next attempt's task, and `retry_policy.repair_prompt_builder` is a
  closure that returns the full replacement task from the retry context
  (`{task, attempt, findings, verification, error, prior_text, stage}`). With
  neither set, retries stay byte-identical to before. The per-stage attempt
  loop now runs in embedded Harn (`std/workflow/stage.harn`); Rust keeps only
  the enforcement/attestation leaves. Added `workflow_repair_stage_graph`
  (`std/workflow/patterns`) — one-stage sugar over the stage retry policy for
  validate→repair loops.
