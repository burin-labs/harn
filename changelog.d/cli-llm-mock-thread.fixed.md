- **CLI LLM mock replay now survives provider worker threads.** `--llm-mock`
  replay and recording scopes are carried on each request, so off-thread
  provider dispatch cannot silently fall through to a real provider.
