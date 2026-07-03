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
#     is a burst of 30 followed by 1/min sustained; this workspace stays
#     below the burst ceiling, so no pre-emptive delay is needed.
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

# harn-cli's package `include` list pulls in portal-dist/ — a gitignored
# build artifact produced by `npm run build` during the release gate. Cargo
# treats its contents as "uncommitted changes" relative to git and refuses
# to package without --allow-dirty. Force the flag on when portal-dist/
# has content so release_ship.sh's publish step doesn't blow up on the
# predictable gitignored-but-included case.
if [[ -z "$ALLOW_DIRTY" && -d "crates/harn-cli/portal-dist" ]] \
  && [[ -n "$(ls -A crates/harn-cli/portal-dist 2>/dev/null)" ]]; then
  ALLOW_DIRTY="--allow-dirty"
  echo "=== portal-dist/ present (gitignored, included by Cargo); publishing with --allow-dirty ==="
fi

RETRY_DELAY=120  # seconds to wait on rate limit
INDEX_SETTLE_DELAY=30  # seconds to wait for index propagation between retries
MAX_ATTEMPTS=3

# Cargo classifies several non-fatal conditions as fatal exits, so the
# bare `cargo publish --workspace` can fail mid-stream without actually
# being broken. We retry on:
#   - 429 / Too Many Requests              — crates.io rate limit, real
#   - "unexpected cargo internal error"    — known cargo bug where it gives
#   - "packages remain in plan"              up waiting on index propagation
#                                            after a successful upload
#   - "already exists on crates.io index" /
#     "crate version ... is already uploaded"
#                                          — crate succeeded on an earlier
#                                            attempt; nothing to do
#   - "timeout while waiting for published  — a dependency was uploaded but the
#      dependencies" / "timed out waiting     index hadn't propagated before
#      for ... to be available"               cargo tried to publish a dependent
#                                            (e.g. harn-cli waiting on harn-lsp).
#                                            Pure propagation lag: retrying or the
#                                            per-crate fallback (which skips the
#                                            already-uploaded crates) completes it.
ALREADY_PUBLISHED_PATTERN='already exists on crates\.io index|crate version .* is already uploaded'
RETRYABLE_PATTERN="429|Too Many Requests|unexpected cargo internal error|packages remain in plan|${ALREADY_PUBLISHED_PATTERN}|timeout while waiting for published dependencies|timed out waiting for"

workspace_publish_crates() {
  cargo metadata --format-version 1 --no-deps | python3 -c '
import json
import sys

meta = json.load(sys.stdin)
workspace = set(meta.get("workspace_members", []))
packages = [
    pkg
    for pkg in meta.get("packages", [])
    if pkg.get("id") in workspace
    and (pkg.get("publish") is None or "crates-io" in pkg.get("publish", []))
]
by_name = {pkg["name"]: pkg for pkg in packages}
deps = {
    pkg["name"]: sorted(
        {
            dep["name"]
            for dep in pkg.get("dependencies", [])
            if dep.get("source") is None and dep.get("name") in by_name
        }
    )
    for pkg in packages
}

visiting = set()
visited = set()
ordered = []


def visit(name):
    if name in visited:
        return
    if name in visiting:
        cycle = " -> ".join(sorted(visiting | {name}))
        raise SystemExit(f"workspace publish dependency cycle: {cycle}")
    visiting.add(name)
    for dep in deps[name]:
        visit(dep)
    visiting.remove(name)
    visited.add(name)
    ordered.append(name)


for name in sorted(by_name):
    visit(name)

print("\n".join(ordered))
'
}

publish_output_means_already_published() {
  local output="$1"
  grep -Eiq "$ALREADY_PUBLISHED_PATTERN" <<<"$output"
}

run_and_capture_output() {
  local __output_var="$1"
  shift
  local output_file
  output_file="$(mktemp "${TMPDIR:-/tmp}/harn-publish-output.XXXXXX")"
  : > "$output_file"

  local command_status=0
  if "$@" > >(tee -a "$output_file") 2> >(tee -a "$output_file" >&2); then
    command_status=0
  else
    command_status=$?
  fi

  printf -v "$__output_var" '%s' "$(cat "$output_file")"
  rm -f "$output_file"
  return "$command_status"
}

crate_version_exists() {
  local crate="$1"
  if ! command -v curl &>/dev/null; then
    return 1
  fi
  curl \
    --fail \
    --silent \
    --show-error \
    --location \
    --retry 3 \
    --retry-delay 2 \
    --header "User-Agent: harn-release-publish" \
    --output /dev/null \
    "https://crates.io/api/v1/crates/${crate}/${CURRENT_VERSION}" \
    >/dev/null 2>&1
}

attempt_workspace_publish() {
  local attempt=1
  while [[ $attempt -le $MAX_ATTEMPTS ]]; do
    echo ""
    echo "=== Publishing workspace (attempt $attempt/$MAX_ATTEMPTS) ==="
    local output
    if run_and_capture_output output cargo publish --workspace $DRY_RUN $VERIFY_FLAGS $ALLOW_DIRTY; then
      return 0
    fi

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
# bailing on the cargo internal error. Walks publishable workspace crates in
# dependency order and treats any crate/version already visible on crates.io as
# success, regardless of Cargo's exact wording.
attempt_per_crate_publish() {
  echo ""
  echo "=== Per-crate publish fallback ==="
  local crate
  local output
  while IFS= read -r crate; do
    [[ -n "$crate" ]] || continue
    echo ""
    echo "--- Publishing $crate ---"
    if crate_version_exists "$crate"; then
      echo "  $crate already published at version $CURRENT_VERSION — skipping"
      continue
    fi
    if run_and_capture_output output cargo publish -p "$crate" $DRY_RUN $VERIFY_FLAGS $ALLOW_DIRTY; then
      continue
    fi
    if publish_output_means_already_published "$output" || crate_version_exists "$crate"; then
      echo "  $crate already published at this version — skipping"
      continue
    fi
    if echo "$output" | grep -q "429\|Too Many Requests"; then
      echo "  Rate limited on $crate. Waiting ${RETRY_DELAY}s and retrying once..."
      sleep "$RETRY_DELAY"
      if run_and_capture_output output cargo publish -p "$crate" $DRY_RUN $VERIFY_FLAGS $ALLOW_DIRTY; then
        continue
      fi
      if publish_output_means_already_published "$output" || crate_version_exists "$crate"; then
        echo "  $crate already published at this version — skipping"
        continue
      fi
    fi
    echo "  FAILED to publish $crate"
    return 1
  done < <(workspace_publish_crates)
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
