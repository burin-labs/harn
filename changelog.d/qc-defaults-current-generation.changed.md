- Quality-check, sitrep, judge and approval-reviewer defaults move to the
  current generation: `claude-haiku-5-5` for Anthropic and `gpt-6-luna` for
  OpenAI. The bare runtime fallbacks move to `claude-sonnet-5-5` and
  `gpt-6-luna`. `std/agent/sitrep` resolves model aliases before adding its
  summary temperature only on a route whose capability row accepts one, so
  routes that reject non-default sampling no longer refuse the summary call.
  Response compaction, prompt refinement, scratchpad reorganization and
  missing-tool classification also leave sampling to the resolved route
  instead of forcing an unsupported temperature. Explicit caller settings
  still receive the provider's capability validation. Decision evaluations
  preserve omitted effort and temperature instead of inventing defaults, allowing
  structured classification on routes that reject sampling parameters.
  Provider capability metadata exposes the supported effort ladder, and
  missing-tool classification chooses the lowest catalog-supported effort
  instead of forcing an unsupported reasoning-off setting. An unknown effort
  ladder leaves reasoning to the provider. Native decision tape identities
  remain unchanged when effort and sampling are omitted.
  The standalone approval reviewer uses
  Haiku 5.5, matching its Anthropic catalog role.
