- Added the behavioral tier of the ASR (attack-success-rate) battery
  (`security::behavioral`): a deterministic, judge-free probe that runs each
  role-confusion attack case through a model as a framed untrusted document and
  scores obedience by a per-case canary token. Where the static battery measures
  detection and containment, this measures the outcome that protects the user —
  whether the model actually obeys an injected directive under the shipped
  `spotlight_wrap` framing. Model access is behind a `BehavioralModel` trait so
  the aggregation is unit-tested with mocks (no network in CI); the live baseline
  is run on demand and is the pre-LoRA number role-robustness training must beat.
