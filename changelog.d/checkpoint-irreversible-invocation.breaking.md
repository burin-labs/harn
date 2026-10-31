- **Workspace checkpoint receipts name each irreversible effect by its invocation.**
  `IrreversibleEffect` gains `invocation`, the session revision at which that call
  began, so two network or desktop-control effects whose provider reused a tool
  call ID stay distinct. Plan and restore responses carry it next to `call_id`.

  Migration: Rust code that builds `IrreversibleEffect` with a struct literal adds
  `invocation`. Readers of the plan or restore JSON that key effects by `call_id`
  should key them by `invocation`; `call_id` remains for correlation.
