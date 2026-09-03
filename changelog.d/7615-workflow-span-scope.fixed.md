- **Workflow trace-span evidence is scoped to its own execution (#7615).** A
  workflow run record filled `evidence.trace_spans` from every completed span on
  the thread, so a span belonging to a different execution could appear in this
  execution's evidence. The workflow writers now select on the owning execution,
  matching the canonical run-record path.
