#!/usr/bin/env bash
# Smoke an already-installed `harn` binary that came from a user-facing
# install channel (`cargo install harn-cli` or `install.sh`) against the
# cross-platform release smoke harness.
#
# The per-tag `release-smoke.yml` gate validates the binary at release
# time. This helper backs `install-smoke.yml`, which catches drift
# *between* releases — a transitive dependency that stops building on
# crates.io, `install.sh` OS/arch detection regressing, a yanked crate —
# that the release-time gate cannot observe because it never re-installs.
#
# It derives the release tag from the binary's reported version, overlays
# `scripts/release_smoke.harn` and the `tests/smoke` fixtures from that tag
# so the harness matches the published binary under test (the working tree
# may be ahead of the latest release), then runs the driver.
#
# Usage: HARN_BINARY=/path/to/harn scripts/smoke_installed_binary.sh
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

HARN="${HARN_BINARY:?HARN_BINARY must point at the installed harn binary}"
if [[ ! -x "$HARN" ]]; then
  echo "::error::install-smoke: harn binary not found or not executable at $HARN"
  exit 1
fi

raw_version="$("$HARN" --version)"
# `harn --version` prints `harn X.Y.Z`; take the first semver-looking token
# so the parse survives banner tweaks (build metadata, extra fields).
version="$(printf '%s\n' "$raw_version" | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -n1)"
if [[ -z "$version" ]]; then
  echo "::error::install-smoke: could not parse a version from '$raw_version'"
  exit 1
fi
tag="v${version}"
echo "::notice::install-smoke: installed harn reports '${raw_version}'; smoking against tag ${tag}"

# Fetch just the matching tag (the checkout action does a shallow clone of
# the triggering ref and omits tags) and overlay only the driver and
# fixtures. Path-scoped checkout leaves this running script untouched.
if ! git fetch --depth 1 origin "refs/tags/${tag}:refs/tags/${tag}"; then
  echo "::error::install-smoke: could not fetch release tag ${tag}; is the release published?"
  exit 1
fi
git checkout "$tag" -- scripts/release_smoke.harn tests

"$HARN" run --no-sandbox scripts/release_smoke.harn -- --candidate "$HARN"
