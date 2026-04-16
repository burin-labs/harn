#!/usr/bin/env bash
set -euo pipefail

# Publish all harn crates to crates.io in a single `cargo publish --workspace`
# invocation. Dependency ordering, the per-crate index wait, and already-
# published skips are all handled by cargo itself (stable since Rust 1.90;
# see https://doc.rust-lang.org/cargo/commands/cargo-publish.html).
#
# Why not publish each crate in a bash loop anymore:
#   - `cargo publish --workspace` orders crates by their intra-workspace
#     dependency graph automatically.
#   - Cargo already blocks each per-crate upload on the crates.io index
#     catching up before moving on, so an artificial `sleep 15` between
#     crates just made releases slower without preventing anything.
#   - crates.io's publish rate limit for new versions of existing crates
#     is a burst of 30 followed by 1/min sustained; with 8 crates we are
#     nowhere near the burst ceiling, so no pre-emptive delay is needed.
#
# Verification:
#   - Dry run (`--dry-run`) passes `--no-verify` because cargo cannot run the
#     staged-build verification for unpublished workspace dependencies.
#   - Real publish also passes `--no-verify` by default: the release gate
#     (`release_gate.sh audit`) already builds the full workspace with
#     clippy + tests, so the staged rebuild inside `cargo publish` is pure
#     latency. Set `HARN_PUBLISH_VERIFY=1` to force verification (slower,
#     but useful when publishing from a machine that has not already run
#     the audit).
#
# Usage:
#   ./scripts/publish.sh             # publish all crates (fast path)
#   ./scripts/publish.sh --dry-run   # verify without uploading
#   HARN_PUBLISH_VERIFY=1 ./scripts/publish.sh   # re-enable cargo's staged
#                                                # build verification

DRY_RUN=""
if [[ "${1:-}" == "--dry-run" ]]; then
  DRY_RUN="--dry-run"
  echo "=== DRY RUN (no uploads) ==="
fi

VERIFY_FLAGS="--no-verify"
if [[ -z "$DRY_RUN" && "${HARN_PUBLISH_VERIFY:-0}" == "1" ]]; then
  VERIFY_FLAGS=""
  echo "=== HARN_PUBLISH_VERIFY=1 set; cargo will run staged-build verification ==="
fi

ALLOW_DIRTY=""
if ! git diff --quiet --ignore-submodules HEAD --; then
  ALLOW_DIRTY="--allow-dirty"
  echo "=== Dirty tree detected; publishing with --allow-dirty ==="
fi

RETRY_DELAY=120  # seconds to wait on rate limit
INDEX_SETTLE_DELAY=30  # seconds to wait for index propagation between retries
MAX_ATTEMPTS=3

# Crates to publish, in dependency order. Used by the per-crate fallback
# when `cargo publish --workspace` bails out partway through.
WORKSPACE_CRATES=(
  harn-lexer
  harn-parser
  harn-fmt
  harn-vm
  harn-lint
  harn-dap
  harn-lsp
  harn-cli
)

# Cargo classifies several non-fatal conditions as fatal exits, so the
# bare `cargo publish --workspace` can fail mid-stream without actually
# being broken. We retry on:
#   - 429 / Too Many Requests              — crates.io rate limit, real
#   - "unexpected cargo internal error"    — known cargo bug where it gives
#   - "packages remain in plan"              up waiting on index propagation
#                                            after a successful upload
#   - "already exists on crates.io index"  — crate succeeded on an earlier
#                                            attempt; nothing to do
RETRYABLE_PATTERN='429|Too Many Requests|unexpected cargo internal error|packages remain in plan|already exists on crates.io index'

attempt_workspace_publish() {
  local attempt=1
  while [[ $attempt -le $MAX_ATTEMPTS ]]; do
    echo ""
    echo "=== Publishing workspace (attempt $attempt/$MAX_ATTEMPTS) ==="
    local output
    if output=$(cargo publish --workspace $DRY_RUN $VERIFY_FLAGS $ALLOW_DIRTY 2>&1); then
      echo "$output"
      return 0
    fi

    echo "$output"

    if echo "$output" | grep -Eq "$RETRYABLE_PATTERN"; then
      if [[ $attempt -lt $MAX_ATTEMPTS ]]; then
        local delay="$INDEX_SETTLE_DELAY"
        if echo "$output" | grep -q "429\|Too Many Requests"; then
          delay="$RETRY_DELAY"
          echo "  Rate limited. Waiting ${delay}s before retry..."
        else
          echo "  Cargo bailed mid-publish (likely index propagation lag). Waiting ${delay}s before retry..."
        fi
        sleep "$delay"
        attempt=$((attempt + 1))
        continue
      fi
      echo "  Workspace publish still failing after $MAX_ATTEMPTS attempts; falling back to per-crate publish"
      return 2  # signal: try per-crate fallback
    fi

    echo "  FAILED to publish workspace (non-retryable error)"
    return 1
  done
  return 2
}

# Per-crate fallback for the case where `cargo publish --workspace` keeps
# bailing on the cargo internal error. Walks crates in dependency order
# and treats "already exists on crates.io index" as success.
attempt_per_crate_publish() {
  echo ""
  echo "=== Per-crate publish fallback ==="
  local crate
  local output
  for crate in "${WORKSPACE_CRATES[@]}"; do
    echo ""
    echo "--- Publishing $crate ---"
    if output=$(cargo publish -p "$crate" $DRY_RUN $VERIFY_FLAGS $ALLOW_DIRTY 2>&1); then
      echo "$output"
      continue
    fi
    if echo "$output" | grep -q "already exists on crates.io index"; then
      echo "  $crate already published at this version — skipping"
      continue
    fi
    if echo "$output" | grep -q "429\|Too Many Requests"; then
      echo "$output"
      echo "  Rate limited on $crate. Waiting ${RETRY_DELAY}s and retrying once..."
      sleep "$RETRY_DELAY"
      if output=$(cargo publish -p "$crate" $DRY_RUN $VERIFY_FLAGS $ALLOW_DIRTY 2>&1); then
        echo "$output"
        continue
      fi
      if echo "$output" | grep -q "already exists on crates.io index"; then
        echo "  $crate already published at this version — skipping"
        continue
      fi
    fi
    echo "$output"
    echo "  FAILED to publish $crate"
    return 1
  done
  return 0
}

CURRENT_VERSION="$(cargo metadata --format-version 1 --no-deps 2>/dev/null \
  | python3 -c "import sys,json; print(json.load(sys.stdin)['packages'][0]['version'])" 2>/dev/null || echo "?")"
echo "Publishing workspace at version $CURRENT_VERSION"
echo ""

set +e
attempt_workspace_publish
ws_status=$?
set -e

if [[ $ws_status -eq 2 ]]; then
  attempt_per_crate_publish
elif [[ $ws_status -ne 0 ]]; then
  exit "$ws_status"
fi

echo ""
echo "=== Workspace publish complete ==="
