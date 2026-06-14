- **gpt-oss / Harmony channel leaks no longer pollute conversation history.**
  On ~23% of gpt-oss-120b turns the provider fails to split its harmony
  channels and collapses the analysis reasoning plus the inline tool call into
  the assistant `content` (empty `reasoning` field, empty native `tool_calls`).
  The bare `{"tool":..,"arguments":..}` dialect emitted in that case is now
  recovered (the native-JSON parser accepts `tool` as a `name` alias, in both
  the acceptance gate and the extractor — previously the call was dropped and
  the loop saw a stall), and the persisted assistant turn is rebuilt into the
  canonical shape a non-leaked turn produces: structured `tool_calls`, the
  leaked trace moved to the private `reasoning` field (stripped from the wire by
  the prior-turn reasoning fix), and empty `content`. This stops the model's raw
  chain-of-thought — including "game the verifier" plans — from being
  re-serialized into every later request and wasting input tokens.
