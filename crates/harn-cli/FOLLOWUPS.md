# Follow-up issues to file

These are scoped out of the initial composable-llm-callers task block. Task #12
will translate them into GitHub issues; do not file them ad hoc.

- **harn models recommend: hardware-aware starter model picker.** Inspect
  `harn doctor`'s `hardware` snapshot (RAM, GPU/MPS, free disk) and propose a
  best-fit model id, prefering local Ollama when no cloud creds are set.
- **harn models test: round-trip a small prompt through any model.** Smoke
  test a single chat completion against `<provider>/<model>` and report
  latency + first-token-time + cost estimate.
- **harn quickstart: interactive setup wizard.** Detect missing creds, walk
  through provider selection, and write a starter `providers.toml` /
  `harn.toml` / `.env`.
- **harn new --template chat: REPL-style chat skeleton.** Project template
  that scaffolds a streaming chat loop wired to the configured provider.
- **harn run --explain-cost: pre-run cost estimate.** Static analysis of an
  agent script that reports projected per-iteration token spend before exec.
- **Shell completions: zsh, bash, fish (clap_complete).** Generate completion
  scripts via `clap_complete` and document install paths.
- **Auto-detect ollama on first run and seed providers config.** When a fresh
  install finds Ollama running locally, write a minimal `providers.toml`
  pinning ollama as the default provider so `harn try` works zero-config.
