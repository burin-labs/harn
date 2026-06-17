- **JSONL agent event logs are now line-durable during live runs.** The flat
  `JsonlEventSink` flushes after each appended event so replay/eval consumers
  can read terminal tail records such as `iteration_end`, `typed_checkpoint`,
  and `judge_decision` before the sink is dropped.
