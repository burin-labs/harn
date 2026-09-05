- **OpenAI GPT-6 Astra is in the provider catalog.** `gpt-6-astra` is
  selectable directly or through the `gpt-6` and `astra` aliases, with a
  1,050,000-token context window, the 128,000-token output ceiling, and pricing
  that carries the same over-272k input band as the GPT-5.6 family (2x input,
  1.5x output applied to the whole request). Its capability row was probed
  against the live API rather than transcribed from the model page, which
  matters in two places: the effort ladder is only `low`/`medium`/`high`/
  `xhigh`, so both `none` and the `max` that OpenAI's own docs advertise are
  rejected, and `temperature` and `top_p` are refused outright. Function tools
  are unusable on `/v1/chat/completions` for this model at any effort, so the
  row carries `reasoning_tools_require_responses` and Harn routes tool-bearing
  turns to the Responses API automatically. Without a model-specific row
  `gpt-6-astra` parses as generation (6, 0) and inherits the generic GPT-5.4+
  rule, which advertises `none` and leaves sampling unconstrained; every agent
  turn under that row would have cost a provider 400.
