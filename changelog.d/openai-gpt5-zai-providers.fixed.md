- Fixed OpenAI `gpt-5.x` dispatch: reasoning models now send
  `max_completion_tokens` instead of the rejected legacy `max_tokens`, so
  `gpt-5.5`/`gpt-5.4`/`gpt-5.2`/`gpt-5.1`/`gpt-5` serve through the chat
  completions path again.
- Fixed OpenAI `*-codex` models (responses-endpoint only): they now
  auto-route through the Responses API instead of returning a silent HTTP 404
  on `/v1/chat/completions`.
- Fixed the Z.AI base URL (`https://api.z.ai/api/paas/v4`; the previously
  catalogued `.../v1` returned 404) and refreshed the GLM catalog to the live
  lineup (`glm-4.5`, `glm-4.5-air`, `glm-4.6`, `glm-4.7`, `glm-5-turbo`).
