- Vertex now delegates Gemini `generateContent` response parsing to the shared Gemini parser, so
  Gemini 2.5 thinking + function-calling over Vertex keeps the `thoughtSignature` it must replay next
  turn, skips empty-name function calls, and reports usage telemetry — matching the direct Gemini path.
- OpenAI-compatible parallel tool-call history splitting now attaches each tool result by the call's own
  id instead of by position, so a batch where some calls lack an id no longer misattaches a result to the
  wrong assistant call.
