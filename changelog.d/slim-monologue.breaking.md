- **Agent loop.** Removed `stall_diagnostics.monologue_actuation` and the
  `std/agent/monologue_actuation` and `std/agent/monologue_actuation_types` modules (#11147).
  Repeated reads and tool-less narration no longer arm a forced write-tool prefill.

  Migration: remove the option and imports of those modules. Let the model choose
  its next tool call within the configured run limits.
