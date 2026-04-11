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
MAX_ATTEMPTS=3

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

    # Rate-limit retry. `cargo publish --workspace` is non-atomic, so if we
    # get a 429 partway through, subsequent retries will skip the already-
    # published crates via cargo's "already exists" handling.
    if echo "$output" | grep -q "429\|Too Many Requests"; then
      if [[ $attempt -lt $MAX_ATTEMPTS ]]; then
        echo "  Rate limited. Waiting ${RETRY_DELAY}s before retry..."
        sleep "$RETRY_DELAY"
        attempt=$((attempt + 1))
        continue
      fi
      echo "  FAILED: still rate limited after $MAX_ATTEMPTS attempts"
      return 1
    fi

    echo "  FAILED to publish workspace"
    return 1
  done
}

CURRENT_VERSION="$(cargo metadata --format-version 1 --no-deps 2>/dev/null \
  | python3 -c "import sys,json; print(json.load(sys.stdin)['packages'][0]['version'])" 2>/dev/null || echo "?")"
echo "Publishing workspace at version $CURRENT_VERSION"
echo ""

attempt_workspace_publish

echo ""
echo "=== Workspace publish complete ==="
