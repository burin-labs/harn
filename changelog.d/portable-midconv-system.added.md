- **Portable mid-conversation `system` / `developer` messages.** A conversation
  may now carry a `system`- or `developer`-role message at any position — an
  operator instruction delivered mid-conversation (a mode switch, a
  runtime-fetched constraint, injected state) — and Harn rewrites it at the wire
  boundary to the exact form the target route accepts, driven by the new
  `system_message_placement` provider capability. Claude Opus 4.8 carries a
  validly-placed message natively as `role: "system"`; OpenAI
  and Ollama carry it inline; Gemini, Bedrock, and older/other Claude fold it
  into the adjacent user turn as a `<system-reminder>` block so its position and
  operator intent survive. The same script now runs on every provider without
  hitting a provider-specific placement `400` or a silently-repositioned
  directive — where before, an interleaved system message worked on OpenAI, was
  hoisted into the global system prompt on Gemini/Bedrock, and was rejected
  outright by Anthropic.
