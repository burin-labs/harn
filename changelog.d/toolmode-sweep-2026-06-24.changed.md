Refreshed provider tool-call wire-format pins from a real-spend forced-format sweep (2026-06-24, N=5).
MiniMax-M2.7 (Together, SambaNova), Kimi-K2.7-Code (OpenRouter), and Qwen3.6-35B-A3B (DeepInfra) corrupt
backslash-heavy file bodies on the provider-native and fenced-JSON channels but round-trip them byte-clean
on the escape-free heredoc `text` channel; those four routes are now pinned `preferred_tool_format = "text"`
/ `tool_mode_parity = "native_unreliable"`. Evidence: `docs/eval/provider-tool-mode-sweep-2026-06-24.md`.
