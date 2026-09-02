- **Two write-only receipt topics removed (#7615).** Lifecycle receipts no longer
  append to the `agent.lifecycle.receipts` event-log topic and git receipts no
  longer append to `stdlib.git.receipts`. Nothing read either topic: lifecycle
  consumers read the in-process journal, and git receipt content is still
  recorded in the trust graph.
