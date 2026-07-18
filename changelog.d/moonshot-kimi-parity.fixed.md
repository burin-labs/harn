- **Provider catalog: Moonshot Kimi K3 tool-mode parity resolved and the rejected forced tool-choice mode removed.**
  A 2026-07-18 credentialed probe (direct Moonshot `/v1`, N=2) upgraded K3's
  `tool_mode_parity` from `unknown` to `interchangeable` and dropped `required`
  from its `allowed_tool_choice_modes`: K3 always reasons, so Moonshot rejects a
  forced `tool_choice` with HTTP 400 "incompatible with thinking enabled". That
  400 bills nothing, so the reported "empty completion / 0 tokens" Kimi
  agent-loop drops were a forced-tool-choice rejection, not a native-emission
  drop. Native+auto and the Harn text channel both carried a
  backslash/quote/unicode large-string argument byte-exact (2/2) for K3, so the
  modes are interchangeable; the general `*kimi*` (k2.5/k2.6) rule stays pinned
  to native because the k2.6 text channel over-ran its budget in the reasoning
  channel and never emitted the tool call.
- **Provider catalog: Fireworks `minimax-m3` tool-mode parity resolved.**
  The same credentialed probe (direct Fireworks `/v1`, N=2) upgraded the
  `minimax-m3` route from `unknown` to `interchangeable`: native emitted
  byte-exact on both `tool_choice: auto` and `required` (2/2) and the text
  channel parsed byte-exact (2/2).
