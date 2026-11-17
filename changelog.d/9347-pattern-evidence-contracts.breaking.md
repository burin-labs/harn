Pattern-learning session readers and observation constructors now retain typed
tool sequences, run groups, command ledgers and normalized observations.
Partial references preserve existing values, nominal containers, extension
fields and conditional malformed-value refusals. Refs #9347.

Migration:

Use `PatternCommandLedgerEntry<TExitCode>`, `SessionRun`, and
`PatternLearningObservation` for generated results. Decode a raw session command's
`exit_code` or the raw `pl_run_goal_outcome` payload before reading typed fields;
those values remain `unknown` because their producer owns their shape. A
sanitized command's `exit_code` is `int?`, so check for `nil` before arithmetic.
Event extensions also require decoding:

```harn,ignore
type EventExtension = {source: string, ...dict<string, unknown>}
const extension = schema_expect(event.extension, schema_of(EventExtension))
harness.stdio.println(extension.source)
```
