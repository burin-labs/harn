- Fireworks `glm-5p2`, `deepseek-v4-flash-0731`, `deepseek-v4-pro`,
  `deepseek-v4-pro-0813`, `kimi-k2p6`, and `kimi-k2p7-code` are now catalogued
  as dedicated-only. Fireworks still lists them, but a serverless chat request
  returns HTTP 404, so equivalent-model substitution no longer picks them. The
  provider contract campaign also skips dedicated-only routes instead of
  recording their 404 as an unmeasured option.
