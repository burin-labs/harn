- **Provider-backed hypothesis planning now avoids false free calls and rejected schema preflights (#6353).**
  Planner output budgets derive from the selected model unless explicitly bounded, Harn unions project to
  provider JSON Schema, and omitted streamed usage remains unknown instead of becoming zero cost.
