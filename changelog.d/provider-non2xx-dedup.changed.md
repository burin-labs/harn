- Collapsed the copy-pasted non-2xx HTTP error handling in the Azure, Bedrock, Gemini, Vertex, and OpenAI
  Responses LLM adapters into a single `err_for_non_success` helper, normalizing cosmetic wrapping drift with
  no change to the surfaced error value.
