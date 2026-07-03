- **Role-hygiene ingress: special-token neutralization + destyling inside the
  spotlight frame.** `spotlight_wrap` now runs two structural passes on an
  untrusted body before framing it: `neutralize_special_tokens` rewrites reserved
  chat-template tokens (`<|im_start|>`, `[INST]`, `<|eot_id|>`, …) to
  `⟦special-token:…⟧` so they cannot re-open turns or inject a system message
  (ChatBug / ChatInject / MetaBreak), and `destyle_untrusted` neutralizes
  line-leading `User:`/`Assistant:`/`System:` labels and `<think>` reasoning tags
  (arXiv:2603.12277) so injected content cannot read as a real turn or
  chain-of-thought. Both are idempotent, surgical (benign look-alikes untouched),
  and default on for every non-`off` mode; new `[security]` knobs
  `neutralize_special_tokens` / `destyle_untrusted` toggle them via
  `std/security::configure`. The ASR battery now proves the delta in one run:
  special-token survival drops from **1.00** (framing only) to **0.00** under the
  default posture, and role-style survival is **0.00** for the tagged/prefixed
  attacks. String-level containment; a tokenizer-level guarantee over rendered
  token IDs is a planned follow-up.
