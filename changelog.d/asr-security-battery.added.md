- **Static ASR battery for the prompt-injection substrate.**
  `harn_vm::security::battery` measures `crate::security` against the
  role-confusion attack classes (arXiv:2603.12277 and the ChatBug / ChatInject /
  MetaBreak lineage) with no model call: `run_static_battery(mode)` reports the
  classifier's under-detection rate, the false-positive rate on benign controls,
  and the special-token survival rate through `spotlight_wrap`. The embedded
  corpus (`security/fixtures/asr-battery.json`) carries CoT-forgery, role-tag
  forgery, special-token smuggling, spotlight breakout, concealment, exfil, and
  cross-agent-poisoning attacks plus benign false-positive controls, each with a
  `injected_directive` / `success_signal` the Burin behavioural tier consumes.
  Baseline pinned 2026-07-02 (heuristic classifier, threshold 50%):
  undetected 0.82, false-positive 0.33, special-token survival 1.00 — the
  quantified headroom for the neural `local-ml` classifier and the
  token-neutralization work.
