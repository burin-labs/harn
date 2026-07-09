- Kept Ollama thinking traces private across native chat and raw-generate
  parsing instead of promoting them into visible text or stream deltas.
- Made OpenAI-compatible reasoning-only text promotion an explicit capability
  opt-in so incomplete provider rows default to keeping reasoning private.
