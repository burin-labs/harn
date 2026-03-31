#!/usr/bin/env bash
set -euo pipefail

# Publish all harn crates to crates.io in dependency order.
# Handles rate limiting with automatic retries.
# Skips crates that are already published at the current version.
#
# Usage:
#   ./scripts/publish.sh          # publish all crates
#   ./scripts/publish.sh --dry-run  # verify without uploading

DRY_RUN=""
VERIFY_FLAGS=""
if [[ "${1:-}" == "--dry-run" ]]; then
  DRY_RUN="--dry-run"
  VERIFY_FLAGS="--no-verify"
  echo "=== DRY RUN (no uploads) ==="
  echo "=== Dry run skips cargo publish verification for unpublished workspace dependencies ==="
fi

ALLOW_DIRTY=""
if ! git diff --quiet --ignore-submodules HEAD --; then
  ALLOW_DIRTY="--allow-dirty"
  echo "=== Dirty tree detected; publishing with --allow-dirty ==="
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

    local output
    output=$(cargo publish -p "$crate" $DRY_RUN $VERIFY_FLAGS $ALLOW_DIRTY 2>&1) && {
      echo "$output"
      echo "  Published $crate"
      local last_crate="${CRATES[${#CRATES[@]}-1]}"
      if [[ -z "$DRY_RUN" && "$crate" != "$last_crate" ]]; then
        echo "  Waiting 15s before next crate..."
        sleep 15
      fi
      return 0
    }

    # Check if already published (not an error)
    if echo "$output" | grep -q "already exists"; then
      echo "  $crate already published, skipping"
      return 0
    fi

    # Check if rate limited
    if echo "$output" | grep -q "429\|Too Many Requests"; then
      if [[ $attempt -lt $max_attempts ]]; then
        echo "  Rate limited. Waiting ${RETRY_DELAY}s before retry..."
        sleep "$RETRY_DELAY"
      else
        echo "  FAILED: still rate limited after $max_attempts attempts"
        return 1
      fi
    else
      echo "$output"
      echo "  FAILED to publish $crate"
      return 1
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
