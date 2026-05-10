#!/usr/bin/env bash
# Sign locally-built harn binaries on macOS so Gatekeeper doesn't show
# "Verifying harn..." popups when agents in different worktrees launch
# them. Single source of truth for the local-build signing step — `make
# build`, `make build-release`, and `make setup` all route through here.
#
# Two-tier signing:
#   1. If the team Developer ID Application identity is in the user's
#      login keychain, sign with it (best — Apple's online verification
#      finds a known good cert and the popup goes away).
#   2. Otherwise, ad-hoc sign (`-s -`) as a degraded fallback. This
#      matches the prior `Makefile` behavior and keeps Gatekeeper happy
#      enough to launch the binary, though the first-launch popup may
#      still appear because there's no Apple-anchored signature.
#
# Idempotent: safe to re-run after every `cargo build`. No-op on non-macOS.
# Signs all three binaries (harn, harn-dap, harn-lsp) in target/{debug,release}.

set -euo pipefail

[ "$(uname)" = "Darwin" ] || exit 0

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

# The Apple-issued canonical name for the team's Developer ID Application
# certificate. Resolves by exact CN; Team ID (8SXG5TMV2X) is embedded for
# clarity. Override with HARN_LOCAL_SIGN_ID for testing.
SIGN_ID="${HARN_LOCAL_SIGN_ID:-Developer ID Application: Burin Labs, LLC (8SXG5TMV2X)}"

mode="dev-id"
if ! security find-identity -v -p codesigning 2>/dev/null | grep -Fq "$SIGN_ID"; then
  mode="ad-hoc"
fi

if [ "$mode" = "ad-hoc" ] && [ "${HARN_LOCAL_SIGN_QUIET:-0}" != "1" ]; then
  echo "scripts/sign_local_macos.sh: Developer ID not in login keychain — falling back to ad-hoc sign."
  echo "  → Import the team .p12 from 1Password ('Developer ID Application: Burin Labs') to upgrade."
fi

# Resolve target dir: respect HARN_DEV_TARGET_DIR or the value baked into
# .cargo/config.toml's [build] target-dir; otherwise default to ./target.
target_dir="${HARN_DEV_TARGET_DIR:-}"
if [ -z "$target_dir" ] && [ -f .cargo/config.toml ]; then
  target_dir="$(awk -F'"' '
    /^\[build\][[:space:]]*$/ { in_build = 1; next }
    /^\[/ { in_build = 0 }
    in_build && /^[[:space:]]*target-dir[[:space:]]*=/ { print $2; exit }
  ' .cargo/config.toml)"
fi
target_dir="${target_dir:-target}"

sign_one() {
  local path="$1"
  if [ "$mode" = "dev-id" ]; then
    if codesign -d --verbose=2 "$path" 2>&1 | grep -Fq "Authority=$SIGN_ID"; then
      return 0
    fi
    codesign --force --options runtime --timestamp \
      --sign "$SIGN_ID" "$path" 2>/dev/null
  else
    codesign -s - -f "$path" 2>/dev/null || true
  fi
  xattr -d com.apple.quarantine "$path" 2>/dev/null || true
}

signed_any=0
for profile in debug release; do
  for bin in harn harn-dap harn-lsp; do
    path="$target_dir/$profile/$bin"
    [ -x "$path" ] || continue
    sign_one "$path"
    signed_any=1
  done
done

if [ "$signed_any" = "1" ] && [ "${HARN_LOCAL_SIGN_QUIET:-0}" != "1" ]; then
  echo "scripts/sign_local_macos.sh: signed harn/harn-dap/harn-lsp in $target_dir/{debug,release} (mode=$mode)."
fi
