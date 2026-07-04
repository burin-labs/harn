- Add computer use (screenshot + mouse/keyboard control) as an opt-in host capability. `harn-hostlib`
  gains a cross-platform `computer` module (Cargo features `computer` / `computer-local`) exposing
  `hostlib_computer_{screenshot,execute,ui_tree,permissions}` over pluggable local (`xcap` capture +
  `enigo` input), helper, and remote (TCP) socket backends that all share one wire protocol. `harn-vm`
  projects a single neutral computer tool onto each provider's native surface — Anthropic
  `computer_20251124` (with the `computer-use-2025-11-24` beta header), OpenAI Responses `computer`, or a
  portable function-schema fallback for other vision models — and carries screenshot tool results back as
  image content blocks. Off by default; gated by model capability (`computer_use_style`) and
  `BURIN_COMPUTER_USE_TRANSPORT`.
