# Agent verification

`agent_loop` consumes declared verification results and caller completion checks.
Configure post-edit and terminal checks with `repair_diagnostics` on `AgentSpec`
or a workflow stage's `model_policy`.

| Field | Default | Behavior |
| --- | --- | --- |
| `post_edit_reverify` | `false` | After an edit following a failed verification, invoke the configured completion verifier before accepting completion. |
| `reserved_terminal_verify` | `false` | At a budget boundary, check an unverified write before returning. |
| `reserved_terminal_verify_iterations` | `2`, minimum `1` | Limit the extra repair turns available after a failed terminal verification. |
| `zero_write_terminal_verify` | `false` | Invoke terminal verification when no source write was observed. |

These controls require a `verify_completion` or `verify_completion_judge`
callback. The loop's existing verification-attempt and resource limits apply.
Presets explicitly enable `post_edit_reverify`; a bare loop adds no post-edit check.

```harn,ignore
const options = {
  ...base,
  repair_diagnostics: {
    post_edit_reverify: true,
    reserved_terminal_verify: true,
    reserved_terminal_verify_iterations: 2,
  },
}
```

The dispatcher identifies verification through the tool's declared evidence
role. The latest completed, executed verification in a batch supplies the
result. `std/verification` owns diagnostic normalization and whether evidence
is fresh and can decide a check. A successful edit or read cannot clear a
failed verification.

Sequential loops can pass `result.repair_checkpoint` as the next loop's
`repair_checkpoint`. `workflow_execute` shares that evidence between stages
with the same session identity and returns `repair_checkpoints` keyed by
session. Pass that map as `options.repair_checkpoints` to continue a workflow.
An unrelated session cannot consume another session's checkpoint.

The checkpoint retains the workspace write revision, the latest declared
verification, and whether that dispatch has been consumed. Reading pending
evidence through `agent_repair_checkpoint_state` in `std/agent/repair` does not
commit it twice. The owning record is `AgentRepairCheckpoint` in
`std/agent/repair_types`.

An unsuccessful terminal result may include `current_failure` with `class`,
`signature`, and `snippet`. A successful completion clears that failure.
