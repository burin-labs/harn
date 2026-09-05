- **The catalog no longer reports OpenAI routes as uncached.** Every OpenAI
  model exported `prompt_cache: false`, including rows whose own `capabilities`
  list said `prompt_caching`, because the catalog projects that field from the
  capability rule rather than from the model row and no OpenAI rule set it.
  Anthropic, Gemini, Mistral, Together, Moonshot, NVIDIA and Meta all did, so
  OpenAI was the sole omission. Consumers reading `prompt_cache` to decide
  whether a route can amortize a long prefix were told no while the provider was
  already caching and billing accordingly; one real run measured 4,104,868
  cache-read tokens on a route the catalog called uncached. The 22 OpenAI rules
  covering GPT-4o and later, the codex tunes, and the o-series now declare
  `prompt_caching`. The two catch-all rules that backstop pre-4o models still do
  not, and a direction control asserts that stays true.
