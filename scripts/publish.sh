#!/usr/bin/env bash
set -euo pipefail

# Publish all harn crates to crates.io in dependency order.
# Handles rate limiting with automatic retries.
#
# Usage:
#   ./scripts/publish.sh          # publish all crates
#   ./scripts/publish.sh --dry-run  # verify without uploading

DRY_RUN=""
if [[ "${1:-}" == "--dry-run" ]]; then
  DRY_RUN="--dry-run"
  echo "=== DRY RUN (no uploads) ==="
fi

# Dependency order: leaves first, CLI last
CRATES=(
  harn-lexer
  harn-parser
  harn-vm
  harn-fmt
  harn-lint
  harn-lsp
  harn-dap
  harn-cli
)

RETRY_DELAY=120  # seconds to wait on rate limit

publish_crate() {
  local crate="$1"
  local attempt=1
  local max_attempts=3

  while [[ $attempt -le $max_attempts ]]; do
    echo ""
    echo "=== Publishing $crate (attempt $attempt/$max_attempts) ==="

    if cargo publish -p "$crate" $DRY_RUN 2>&1; then
      echo "  Published $crate"
      # Wait a bit between publishes to avoid rate limits
      if [[ -z "$DRY_RUN" && "$crate" != "${CRATES[-1]}" ]]; then
        echo "  Waiting 15s before next crate..."
        sleep 15
      fi
      return 0
    else
      if [[ $attempt -lt $max_attempts ]]; then
        echo "  Rate limited or error. Waiting ${RETRY_DELAY}s before retry..."
        sleep "$RETRY_DELAY"
      else
        echo "  FAILED to publish $crate after $max_attempts attempts"
        return 1
      fi
    fi
    attempt=$((attempt + 1))
  done
}

echo "Publishing $(cargo metadata --format-version 1 --no-deps 2>/dev/null | python3 -c "import sys,json; print(json.load(sys.stdin)['packages'][0]['version'])" 2>/dev/null || echo "?")"
echo "Crate order: ${CRATES[*]}"
echo ""

for crate in "${CRATES[@]}"; do
  publish_crate "$crate"
done

echo ""
echo "=== All crates published ==="
