# Pattern-learning evidence records

`std/agent/pattern_knowledge_sessions` reads run evidence from the canonical
session store. `std/agent/pattern_knowledge_persistence` normalizes that evidence
for pattern memory. These modules retain the following result contracts:

| Result | Type | Meaning |
|---|---|---|
| `pl_session_tool_sequence` | `list<string>` | Normalized tool names in observation order. |
| `pl_session_command_ledger` | `list<PatternCommandLedgerEntry<unknown>>` | Commands with the host's original exit-code value and the existing computed `ok` flag. |
| `pl_sanitize_command_ledger` | `list<PatternCommandLedgerEntry<int?>>` | Redacted commands with integer exit codes, or `nil` when no integer was reported. |
| `pl_session_runs` | `list<SessionRun>` | Run identifiers and their original events. |
| `pl_run_goal_outcome` | `unknown` | The producer's original outcome payload, or `nil`. |
| `pl_observation_from_input`, `pl_observation_from_legacy` | `PatternLearningObservation` | The normalized observation before memory storage adds its fields. |

`PatternCommandLedgerEntry<TExitCode>` contains `tool: string`, `command: string`,
`exit_code: TExitCode`, and `ok: bool`. The raw session reader preserves malformed
or extended exit-code values. The persistence sanitizer keeps only integer codes
and retains the existing success calculation.

`SessionRun` contains `run_id: string` and `events: list<SessionRunEvent>`.
Each retained event has `headers.run_id: string`; other event and header fields
remain `unknown`. Grouping retains nominal record containers and extension
fields. Empty run identifiers are omitted.

`PatternLearningObservation` contains string identifiers, provenance, prompt and
time fields, a string tool sequence, a normalized command ledger, and nullable
`outcome: PatternLearningGoalOutcome?`. The normalized outcome contains
`kind: string` and `reason: string`. The session reader's raw outcome stays opaque
because the completion producer owns its full shape.

Partial session references accept dictionary and nominal record containers.
String fields are validated when consumed; unused malformed fields do not cause
an earlier refusal. The bounded transcript reader still requires a record for
each transcript entry and redacts text and nested tool inputs before clipping.

See [Cross-session pattern knowledge](../concepts/cross-session-pattern-knowledge.md)
for storage and recall policy.
