#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp_root="$(mktemp -d)"
trap 'rm -rf "$tmp_root"' EXIT

mock_bin="$tmp_root/bin"
target_dir="$tmp_root/target"
codesign_log="$tmp_root/codesign.log"
mkdir -p "$mock_bin" "$target_dir/debug"

cat > "$mock_bin/uname" <<'MOCK'
#!/usr/bin/env bash
echo Darwin
MOCK

cat > "$mock_bin/security" <<'MOCK'
#!/usr/bin/env bash
echo '  1) ABCDEF "Developer ID Application: Burin Labs, LLC (8SXG5TMV2X)"'
MOCK

cat > "$mock_bin/codesign" <<'MOCK'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${MOCK_CODESIGN_LOG:?}"
if [[ "${1:-}" == "-d" ]]; then
  exit 1
fi
if [[ "${1:-}" == "--force" ]]; then
  exit "${MOCK_DEV_ID_STATUS:-0}"
fi
exit 0
MOCK

cat > "$mock_bin/xattr" <<'MOCK'
#!/usr/bin/env bash
exit 0
MOCK

cat > "$target_dir/debug/harn" <<'BIN'
#!/usr/bin/env bash
exit 0
BIN
chmod +x "$mock_bin"/* "$target_dir/debug/harn"

run_signer() {
  PATH="$mock_bin:$PATH" \
    CARGO_TARGET_DIR="$target_dir" \
    MOCK_CODESIGN_LOG="$codesign_log" \
    MOCK_DEV_ID_STATUS="$1" \
    HARN_LOCAL_SIGN_QUIET=1 \
    "$repo_root/scripts/sign_local_macos.sh"
}

run_signer 0
grep -Fq -- '--force --options runtime --timestamp --sign Developer ID Application: Burin Labs, LLC (8SXG5TMV2X)' "$codesign_log"
if grep -Fq -- '-s - -f' "$codesign_log"; then
  echo "successful Developer ID signing must not be replaced with an ad-hoc signature" >&2
  exit 1
fi

: > "$codesign_log"
run_signer 1
grep -Fq -- '--force --options runtime --timestamp --sign Developer ID Application: Burin Labs, LLC (8SXG5TMV2X)' "$codesign_log"
grep -Fq -- '-s - -f' "$codesign_log"

# Real-codesign controls: a swallowed signing failure must not read as success.
if [[ "$(/usr/bin/uname)" == Darwin ]]; then
  real_root="$tmp_root/real"
  mkdir -p "$real_root/bin" "$real_root/target/debug"
  cp /usr/bin/true "$real_root/target/debug/harn"
  /usr/bin/codesign --force --sign - "$real_root/target/debug/harn" >/dev/null 2>&1
  xcrun vtool -set-build-version macos 14.0 14.0 -replace \
    -output "$real_root/target/debug/harn" "$real_root/target/debug/harn" >/dev/null 2>&1
  if /usr/bin/codesign --verify --strict "$real_root/target/debug/harn" >/dev/null 2>&1; then
    echo "fixture mutation did not invalidate the signature" >&2
    exit 1
  fi
  real_signer() {
    PATH="$real_root/bin:$PATH" CARGO_TARGET_DIR="$real_root/target" \
      HARN_LOCAL_SIGN_ID="no such identity" HARN_LOCAL_SIGN_QUIET=1 \
      "$repo_root/scripts/sign_local_macos.sh"
  }
  # Negative control: signing "succeeds" without sealing anything.
  cat > "$real_root/bin/codesign" <<'MOCK'
#!/usr/bin/env bash
case "${1:-}" in
  --verify|-d) exec /usr/bin/codesign "$@" ;;
  *) exit 0 ;;
esac
MOCK
  chmod +x "$real_root/bin/codesign"
  if real_signer >/dev/null 2>&1; then
    echo "a signer that left an invalid signature must fail" >&2
    exit 1
  fi
  # Positive control: the real tool repairs the same binary.
  rm "$real_root/bin/codesign"
  real_signer
  /usr/bin/codesign --verify --strict "$real_root/target/debug/harn"
fi

echo "local macOS signing fallback tests passed"
