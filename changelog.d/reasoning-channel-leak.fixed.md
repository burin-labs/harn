- **Reasoning-only turns that also call a tool no longer leak the model's
  private chain-of-thought into the visible message channel.** OpenAI-compatible
  normalization (both the non-streaming `normalize_openai_message_text` and the
  streaming transport path) promoted a turn's extracted reasoning into `.text`
  whenever the content channel was empty — intended for models that legitimately
  answer inside the reasoning channel. But gpt-oss / harmony models route their
  analysis channel into `reasoning_content` and emit a tool call with no
  committed content, so that promotion surfaced their intermediate
  chain-of-thought ("We need to inspect parser.rs first…") as the assistant
  message. That contaminated both the user-facing transcript and the
  transcript-mined eval grader. Promotion is now suppressed when the turn
  carries a tool call (the tool call is the action, the reasoning is not a final
  answer); the reasoning stays under `thinking`. Reasoning-as-answer promotion
  on tool-call-free clean stops is unchanged.
