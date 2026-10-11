- Quality-check, sitrep, judge and approval-reviewer defaults move to the
  current generation: `claude-haiku-5-5` for Anthropic and `gpt-6-luna` for
  OpenAI. The bare runtime fallbacks move to `claude-sonnet-5-5` and
  `gpt-6-luna`. `std/agent/sitrep` now adds its summary temperature only on a
  route whose capability row accepts one, so these routes, which reject
  non-default sampling, no longer refuse the summary call.
  Response compaction also leaves sampling to the resolved route instead of
  forcing an unsupported temperature. The standalone approval reviewer uses
  Haiku 5.5, matching its Anthropic catalog role.
