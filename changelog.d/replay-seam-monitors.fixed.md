- **Make monitor replay detection hermetic under an ambient `HARN_REPLAY`.**
  The monitor and waitpoint stdlib primitives now share one replay-detection
  owner in the dispatcher, which combines the active dispatch signal, the
  `#[cfg(test)]` override seam, and the `HARN_REPLAY` env read. Previously only
  waitpoints had the test seam, so an inherited `HARN_REPLAY` in the invoking
  shell silently flipped monitor tests into replay mode.
