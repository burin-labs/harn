- **Workspace checkpoints can require known writer evidence.** Strict
  sessions refuse to restore scan-only changes until the host confirms the
  exact observed file state.

  Migration: Rust code constructing `CaptureReceipt`, `AttributedChange`,
  `PlanEntry`, or `PlanReceipt` literals must provide their new writer-policy,
  writer, writers, observed-state, or session fields. Hosts selecting `require_known`
  must verify the capture acknowledgement and present unknown-writer paths for
  confirmation before supplying `confirmed_paths`.
