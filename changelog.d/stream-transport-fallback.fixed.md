- **LLM stream transport fallback.** Agent LLM calls that hit a mid-stream
  response-body/read failure now retry once through non-streaming
  request/response transport when the selected route does not require
  streaming, preventing provider stream glitches from masquerading as agent
  convergence failures.
