#!/usr/bin/env bash
set -euo pipefail

if [[ "${RUN_PROMPT_PROSE_RATCHET:-false}" == "true" ]]; then
  make lint-no-rust-prompt-prose
else
  echo "Skipping prompt-prose ratchet (no protected prompt paths changed)."
fi

# Cargo does not inspect source contents once its timestamp-based fingerprint
# says a unit is fresh, and it does not replay diagnostics from that prior
# compile. A restored target/build directory can therefore contain a
# warning-clean workspace unit whose artifact is newer than changed checkout
# source; the strict invocation below then exits successfully without running
# Clippy on that unit. Keep dependency artifacts warm, but invalidate every
# workspace package at the lint boundary so the proof always reaches Clippy.
cargo clean --workspace
cargo clippy --workspace --all-targets -- -D warnings
