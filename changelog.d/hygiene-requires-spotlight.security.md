- **Hygiene passes require spotlight framing.** `SecurityPolicy::from_config`
  now gates `neutralize_special_tokens` and `destyle_untrusted` on
  `spotlight_external`. Both passes run only inside `spotlight_wrap`, which the
  agent host invokes solely under `if policy.spotlight_external`, so "hygiene on,
  spotlight off" was an inert combination that additionally made `policy_summary`
  misreport the active posture. Gating them on their framing prerequisite
  (mirroring the file/command-provenance and precise/trifecta gates) removes the
  nonsensical subset while preserving the meaningful granularity — toggling a
  hygiene pass off *within* spotlight. Default posture is byte-identical.
