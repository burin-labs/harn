- Refreshed the bundled provider/model catalog against provider docs verified
  2026-07-02:
  - **Anthropic**: added Claude Sonnet 5 (direct + OpenRouter rows, intro
    pricing through 2026-08-31) and new `sonnet5`/`sonnet46` aliases; bumped
    the `sonnet`/`frontier` aliases to `claude-sonnet-5`; 4.6+ rows now carry
    their 1M-token context window (the long-context beta graduated to standard
    pricing) and `vision`; marked Claude Opus 3 retired (2026-01-05) and dated
    the Opus 4.1 retirement (2026-08-05); removed the erroneous
    `claude-sonnet-4-7` row (no such model — only the Opus line had a 4.7).
  - **OpenAI**: added the GPT-5.4 tier family (base/mini/nano) and
    GPT-5.3-Codex; deprecated o1/o1-mini/o3/o3-mini with their announced
    shutdown dates; `mid` tier alias and the openai QC default moved off
    gpt-4o-mini to gpt-5.4-mini / gpt-5.4-nano.
  - **Gemini**: fixed gemini-2.5-flash pricing (it carried Flash-Lite's
    $0.10/$0.40 rate; real rate $0.30/$2.50) and gemini-2.5-pro output/cache
    pricing and context window; added gemini-2.5-flash-lite,
    gemini-3.1-pro-preview, and gemini-3.1-flash-lite with capability rules;
    `small` tier alias moved off the stale OpenRouter Qwen3.5-9B route to
    gemini-2.5-flash-lite.
  - **Mistral**: added Codestral 25.08 and Devstral 2 Medium/Small rows,
    `codestral-*`/`magistral-*` inference rules, and a codestral capability
    rule.
  - **Open-weight hosts**: added DashScope first-party Qwen rows
    (`dashscope/<wire-id>` keys), OpenRouter Qwen3-Coder-Next and
    Qwen3.5-397B-A17B, Groq Qwen3.6-27B (with capability rule), Together and
    Fireworks MiniMax M3 / Kimi K2.7-Code, DeepInfra DeepSeek V4 Flash, and
    Z.AI GLM-4.7-Flash (free tier, now the zai QC default).
  - **Pricing corrections**: Moonshot Kimi K2.7-Code ($0.95/$4.00),
    DeepInfra DeepSeek V4 Pro / Kimi K2.7-Code, Fireworks DeepSeek V4 Pro,
    Z.AI GLM-5 ($1.00/$3.20), MiniMax M3 billed rate ($0.30/$1.20 permanent
    50%-off list) with weights now open (HF) and SWE-bench Pro 59.0.
  - **Deprecations**: Groq llama-3.3-70b-versatile (retires 2026-08-16);
    Together GLM-5.2 context corrected to its 262K per-host cap.
  - QC defaults: `local` moved off the sunset hosted `gpt-4o` id to
    `gemma-4-26b-a4b-it`; added a `gemini` QC default.
  - Capability fragments migrated the legacy `json_schema` field to the
    canonical `structured_output` name.
