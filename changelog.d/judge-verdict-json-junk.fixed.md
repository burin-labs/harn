- **Judge verdicts no longer carry trailing JSON junk.** When a structured completion/step judge
  emits sloppy JSON (double commas, run-on key/value pairs) that the structured-call repair layer
  salvages, the captured `verdict` string could include trailing junk — observed live in
  `judge_decision` events as `continue",,` and `continue",  "reasoning":`. `__judge_classify_verdict`
  now normalizes the captured verdict to its leading token (cut at the first JSON structural
  character), so stored/emitted `judge_decision` / `step_judge_decision` verdicts are clean tokens
  and a mangled PASS token (`done",,`) classifies as a pass instead of being wrongly vetoed.
  Multi-word prose verdicts without JSON junk pass through unchanged.
