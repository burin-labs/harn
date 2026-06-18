- **Provider-native tool-call normalization.** Harmony marker-wrapper tool names
  such as `<|constrain|>json` now normalize command-shaped calls before policy
  checks, preventing valid provider-native tool calls from tripping tool-ceiling
  enforcement before dispatch.
