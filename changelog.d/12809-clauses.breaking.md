- **Command stage results retain failed-clause context without mixing diagnostics into verifier output (#11379).**
  Supported shell chains preserve literal setup, environment exports and file
  guards. `command_output_plan` exposes the same clause selection, and optional
  caller-supplied diagnostic strings preserve useful failure messages when stage
  capture is unavailable. Missing or unconfirmed observations remain unmeasured.

  Migration: Rust callers constructing `OutputCapture::PipeWithStage` must supply
  `clause_diagnostics: None` to retain the existing behavior, or provide one plain
  diagnostic string per planned clause. Harn callers can continue using
  `capture: {final_stage: true}` unchanged.
