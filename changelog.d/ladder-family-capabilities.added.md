The canonical model-ladder step (`ModelLadderStepDef` and the `ModelLadderStep`
alias) now carries two optional, serde-absent-when-unset fields: `family`
(e.g. `"haiku"`/`"sonnet"`) and `capabilities` (e.g. `["vision", "tools"]`).
The addition is purely additive and backward-compatible — existing bundles and
records serialize byte-identically — and lets downstreams such as harn-cloud's
`FreeTierRoute` adopt the canonical ladder-step type instead of maintaining
their own copy.
