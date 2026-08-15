Baseten's GLM routes now use the provider's native tool channel instead of
Harn's text grammar. The text pin came from a 2026-06-23 probe that saw GLM-5.2
leak `<tool_call>` markup into assistant content; a re-probe with a Baseten
credential found 16/16 clean `message.tool_calls` across sync and streaming with
`tool_choice` both `auto` and `required`, so the pin is retired. No route in the
catalog still carries that rationale.

The same live sweep retired four Baseten rows the provider now answers with
HTTP 410 (`zai-org/GLM-5`, `zai-org/GLM-5.1`, `moonshotai/Kimi-K2.5`,
`nvidia/Nemotron-120B-A12B`, along with the `baseten-nemotron-super` alias that
pointed at the last of them), added the served replacements
(`zai-org/GLM-5.2-Fast` and the dated `DeepSeek-V4-Pro-0813` /
`DeepSeek-V4-Flash-0731` snapshots), and corrected context windows and sampling
support that had drifted from what Baseten reports: GLM-5.2 serves 1M tokens
rather than the catalogued 256K, DeepSeek V4 Pro 256K rather than 131K, and the
Kimi, DeepSeek, and GPT-OSS routes accept neither `top_p` nor `top_k`.

A retired route is no longer indistinguishable from a failed call. Tool probes
classify HTTP 404/410, and typed `model_not_available`-class error codes, as
`route_unavailable`, and `provider tool-scorecard` reports it as its own issue
with its own counter. Retrying never fixes a catalog row the provider deleted,
so the two need different verdicts.
