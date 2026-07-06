- Honor `output_schema` on `agent_loop`/`agent_turn`: it now gates the loop's
  final answer instead of being silently ignored. The schema is applied only to
  the terminal answer (never forced on every mid-loop turn, where it fought
  tool-calling); off-shape answers are re-asked once through the `llm_caller`
  seam, and the parsed value is surfaced on `run.output` with `run.output_valid`
  recording whether validation passed.
