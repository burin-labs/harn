- Plain calls to OpenRouter's Grok 4.5, 4.6, and 4.7, GPT-5.4 Pro, GPT-5.5 Pro, and Kimi K2.7 Code no
  longer fail with HTTP 400 "Reasoning is mandatory": Harn stopped sending a reasoning-disable those routes
  reject.
- OpenRouter routes no longer claim sampling options that no endpoint forwards, so a caller's temperature,
  top_p, seed, penalty, or stop sequence on those routes is refused locally instead of silently dropped.
  This covers the Claude, GPT-5.4 to GPT-6.1, Gemini 3.x, Grok, GLM 5.3 FlashX, GLM-5V Turbo, Seed 2.0
  Lite, and free Nemotron rows. OpenRouter therefore cannot run GPT-5.4 Mini, GPT-5.4 Nano, or Claude
  Fable decisions at the structured profile's temperature 0.
- `top_k` is now admitted on the OpenRouter Qwen, Kimi, MiniMax, GLM-5, Step 3.7 Flash, Cohere North Mini
  Code, and `openrouter/free` routes, all of which serve it. Kimi K2.7 Code on OpenRouter now admits
  temperature and top_p, and the free Nemotron route is declared to train on prompts.
