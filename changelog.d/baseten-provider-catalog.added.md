- **Provider catalog and Baseten Model APIs.** Added Baseten as a first-class
  OpenAI-compatible provider with current GLM/Kimi/DeepSeek/GPT-OSS/Nemotron
  routes, rate-limit and serving-performance metadata in the exported catalog
  contract, live catalog refresh hooks for additional SOTA OpenAI-compatible
  providers, cross-platform llama.cpp setup guidance, and provider-tool-probe
  alias handling for catalog rows that need provider-native `wire_model` IDs.
