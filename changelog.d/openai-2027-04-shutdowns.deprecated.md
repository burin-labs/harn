- The model catalog marks `gpt-5.4-nano`, `gpt-5.3-codex`, and `gpt-5.1` deprecated
  with OpenAI's 2027-04-01 shutdown date and its named replacements: `gpt-6-luna` for
  Nano and `gpt-6-sol` for the other two. `gpt-5.1` gains a catalog row so its pricing
  and shutdown are visible, and the OpenRouter and Vercel AI Gateway Nano routes carry
  the same sunset. Decision evaluations still refuse `gpt-6-luna`, because the
  structured profile requires temperature 0; use `gpt-5.4-mini` for OpenAI decisions
  after Nano retires.
