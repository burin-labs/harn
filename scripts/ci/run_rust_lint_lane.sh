#!/usr/bin/env bash
set -euo pipefail

if [[ "${RUN_PROMPT_PROSE_RATCHET:-false}" == "true" ]]; then
  make lint-no-rust-prompt-prose
else
  echo "Skipping prompt-prose ratchet (no protected prompt paths changed)."
fi

cargo clippy --workspace --all-targets -- -D warnings
