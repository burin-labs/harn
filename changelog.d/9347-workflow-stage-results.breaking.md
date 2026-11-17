- **Workflow stage helpers now check their result fields (#9347).** Condition, verification, outcome, artifact proposal,
  retry, and execution results have explicit types. Runtime values and extra options are preserved, but source that treats
  optional lists as present or reads custom fields from opaque payloads must change.

  Migration: replace direct iteration over `verdict.findings` or `verdict.checks` with an empty-list fallback. Decode custom
  payloads before reading their fields, and validate handled static results before requiring their payload.

  ```harn
  import { WorkflowPreparedStage } from "std/workflow/options"
  import {
    workflow_evaluate_verification,
    workflow_prepare_static_stage,
    workflow_stage_attempt_outcome,
  } from "std/workflow/stage"

  type CustomVerification = {label: string}

  fn main(harness: Harness) {
    const verdict = workflow_evaluate_verification()
    const findings = verdict.findings ?? []
    const checks = verdict.checks ?? []
    require findings == [], "no findings are expected"
    require checks == [], "no checks are expected"
    const attempt = workflow_stage_attempt_outcome(
      {node: {kind: "stage"}, verification: {label: "custom"}},
    )
    const metadata = schema_expect(attempt.verification, schema_of(CustomVerification))
    require metadata.label == "custom", "custom metadata must survive"
    const raw = workflow_prepare_static_stage(
      harness.agent,
      harness.interaction,
      harness.tools,
      {node: {kind: "fork"}},
    )
    const prepared = schema_expect(raw, schema_of(WorkflowPreparedStage))
    require prepared.result.text == "forked", "the fork payload must be present"
    harness.stdio.println("stage result migration assertions reached")
  }
  ```
