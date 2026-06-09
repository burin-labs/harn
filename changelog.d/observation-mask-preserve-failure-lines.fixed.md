- `harn-vm` observation-mask compaction no longer shreds structured failure
  detail. Masking a large tool output (`default_mask_tool_result`) used a
  weaker, divergent filter than the microcompact path and dropped assertion-
  value lines (`left:`/`right:`/`expected:`/`actual:`/`got`/`want`), rustc
  continuation lines (`-->`, `= help:`, numbered source rows, `^` carets), and
  `Lnnn:` failing-line markers — so the model re-read a summary with the
  actual-vs-expected values removed. There is now ONE shared failure-signal
  filter (`is_failure_signal_line`) used by both the microcompact and
  observation-mask paths; the mask preserves those failure lines (bounded)
  alongside the first-line preview.
