The canonical model-ladder step (`ModelLadderStepDef` and the `.harn`
`ModelLadderStep` alias) now reach full parity: alongside `model`,
`provider?`, and `label?` they carry `when?`, `options?`, `family?`, and
`capabilities?`. All added fields are optional and serde-absent-when-unset, so
existing catalog bundles and records serialize byte-identically. Catalog
`[model_ladders.*]` steps now honor per-step `options` overrides identically to
inline `models:` steps (previously the catalog path silently discarded them),
and `family`/`capabilities` let downstreams such as harn-cloud's
`FreeTierRoute` adopt the canonical ladder-step type instead of maintaining
their own copy.
