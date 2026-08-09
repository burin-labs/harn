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

echo "local macOS signing fallback tests passed"
