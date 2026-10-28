- **Workspace checkpoints can require known writer evidence.** Strict
  sessions require confirmation of scan-only changes and currently refuse
  replacement or deletion of existing files to protect concurrent edits.

  Migration: Rust code constructing `CaptureReceipt`, `AttributedChange`,
  `PlanEntry`, or `PlanReceipt` literals must provide their new writer-policy,
  writer, writers, observed-state, or session fields. Hosts selecting `require_known`
  must verify the capture acknowledgement and present unknown-writer paths for
  confirmation before supplying `confirmed_paths`.
