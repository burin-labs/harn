- **OpenAI-compatible native streaming tool calls.** Added regression coverage
  ensuring streamed `container.exec` argv chunks finalize as canonical `run`
  tool calls instead of surfacing provider-native arguments.
