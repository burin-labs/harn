- **Equivalent LLM failover can now opt into no-dispatch upstream contract violations.**
  `equivalent_failover: {on_no_dispatch: true}` lets same-logical-model
  fallback routes advance after the normal empty-completion retry is exhausted
  for billed no-dispatch provider responses.
