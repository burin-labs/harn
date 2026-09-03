- The model catalog can declare `reasoning_modes`: provider knobs that change
  how much work a model does before answering, as opposed to `serving_tiers`,
  which change how fast the same work is served. The two are independent and a
  request may set both. Selected per call with `reasoning_mode`, validated
  against the catalog, and injected by path so a nested knob merges beside the
  caller's other settings instead of replacing them.
- GPT-5.6 Sol, Terra, and Luna declare OpenAI's `pro` reasoning mode. Pro is
  Responses-API only and is not a separate model id, so `reasoning_mode: "pro"`
  on the existing row is the whole opt-in. OpenAI bills pro at each row's
  standard token rates and charges for the extra work, so the row records a
  measured `token_multiplier` rather than a price override.
