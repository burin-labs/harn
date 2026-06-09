- **Agent tool-call parser no longer silently drops valid calls in two cases.**
  A single stray/unmatched backtick in model prose used to flip the bare-call
  scanner's inline-code flag for the rest of the response, suppressing every
  later bare `name({ ... })` tool call and stalling the agent loop; the flag now
  resets at each newline (Markdown inline code never spans lines). And the
  native-JSON fallback now accepts the flat OpenAI on-the-wire envelope whose
  `arguments`/`parameters` value is a JSON *string*
  (`{"name":"read","arguments":"{\"path\":\"a\"}"}`, common from local
  llama.cpp/vLLM/Ollama OpenAI-mimic templates) — the acceptance gate previously
  required an object and dropped the call even though the extractor already
  decoded the string.
