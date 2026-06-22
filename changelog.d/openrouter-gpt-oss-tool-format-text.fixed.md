Pin OpenRouter-hosted `openai/gpt-oss-*` to the `text` (heredoc) tool channel
instead of `json`. The provider-native channel bills noncommittal on this
aggregate route, so it already rode a TEXT channel; between the two text
grammars, an empirical A/B (real OpenRouter calls, task = author a
backslash-heavy Zig file) showed `text` beats `json` on both dispatch (3/3 vs
2/3) and byte-fidelity (3/3 clean vs 0/3) — gpt-oss double-escapes the
backslashes a JSON string arg requires and corrupts `\\`-heavy source bodies,
while the escape-free heredoc carries them verbatim. Same class as the Fireworks
GPT-OSS flip; direct Cerebras/Groq/DeepInfra GPT-OSS rows keep `native`.
