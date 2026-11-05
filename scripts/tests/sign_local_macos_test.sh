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
path="${@: -1}"
if [[ "${1:-}" == "--verify" && "${MOCK_SEAL_MODE:-0}" == 1 ]]; then
  cmp -s "$path" "$path.seal"
  exit $?
fi
if [[ "${1:-}" == "-d" ]]; then
  exit 1
fi
if [[ "${1:-}" == "--force" ]]; then
  status="${MOCK_DEV_ID_STATUS:-0}"
  if [[ "$status" == 0 && "${MOCK_SEAL_MODE:-0}" == 1 && "${MOCK_SIGN_DROP:-0}" != 1 ]]; then
    cp "$path" "$path.seal"
  fi
  exit "$status"
fi
if [[ "${1:-}" == "-s" && "${MOCK_SEAL_MODE:-0}" == 1 && "${MOCK_SIGN_DROP:-0}" != 1 ]]; then
  cp "$path" "$path.seal"
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

# The resolver can return a binary outside the target selected by a separate
# environment variable. The caller's exact artifact must still be signed.
required="$tmp_root/resolved/debug/harn"
mkdir -p "${required%/*}"
cp "$target_dir/debug/harn" "$required"
cp "$target_dir/debug/harn" "$tmp_root/resolved/debug/harn-freshness-check"
: > "$codesign_log"
PATH="$mock_bin:$PATH" HARN_DEV_TARGET_DIR="$target_dir" CARGO_TARGET_DIR="$tmp_root/resolved" \
  MOCK_CODESIGN_LOG="$codesign_log" HARN_LOCAL_SIGN_QUIET=1 \
  "$repo_root/scripts/sign_local_macos.sh" "$required"
if ! grep -Fq -- "--sign Developer ID Application: Burin Labs, LLC (8SXG5TMV2X) $required" "$codesign_log"; then
  echo "signer did not sign the exact resolver artifact" >&2
  exit 1
fi
if ! grep -Fq -- "--sign Developer ID Application: Burin Labs, LLC (8SXG5TMV2X) $tmp_root/resolved/debug/harn-freshness-check" "$codesign_log"; then
  echo "signer missed the resolved target's freshness checker" >&2
  exit 1
fi
if grep -Fq -- "$target_dir/debug/harn" "$codesign_log"; then
  echo "signer used a conflicting target instead of the resolved target" >&2
  exit 1
fi

# Exercise the actual build target with a stub resolver and a verifier whose
# seal changes when the receipt producer mutates the resolved executable.
caller_root="$tmp_root/caller"
mkdir -p "$caller_root/scripts" "$caller_root/.githooks" "$caller_root/actual/debug" "$caller_root/wrong/debug"
cp "$repo_root/Makefile" "$caller_root/Makefile"
cp "$repo_root/scripts/sign_local_macos.sh" "$caller_root/scripts/sign_local_macos.sh"
cp "$repo_root/.githooks/lib.sh" "$caller_root/.githooks/lib.sh"
cat > "$caller_root/scripts/harn_bin.sh" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == "--print" ]]; then
  printf '%s\n' "${MOCK_RESOLVED_BIN:?}"
elif [[ "${1:-}" == "--record-receipt" && "${MOCK_MUTATE_AFTER_SIGN:-0}" == 1 ]]; then
  printf 'changed\n' >> "${MOCK_RESOLVED_BIN:?}"
fi
MOCK
chmod +x "$caller_root/scripts/harn_bin.sh"
caller_bin="$caller_root/actual/debug/harn"
cp "$target_dir/debug/harn" "$caller_bin"
cp "$target_dir/debug/harn" "$caller_root/wrong/debug/harn"
run_build() {
  PATH="$mock_bin:$PATH" HARN_DEV_TARGET_DIR="$caller_root/wrong" \
    CARGO_TARGET_DIR="$caller_root/actual" MOCK_CODESIGN_LOG="$codesign_log" \
    MOCK_SEAL_MODE=1 HARN_LOCAL_SIGN_QUIET=1 MOCK_RESOLVED_BIN="$caller_bin" \
    make -s -C "$caller_root" build-harn
}
run_build
cmp "$caller_bin" "$caller_bin.seal"
if MOCK_MUTATE_AFTER_SIGN=1 run_build >/dev/null 2>&1; then
  echo "build accepted a signature invalidated while recording the receipt" >&2
  exit 1
fi
rm "$caller_bin.seal"
if MOCK_SIGN_DROP=1 run_build >/dev/null 2>&1; then
  echo "build accepted a signer that did not seal the resolved executable" >&2
  exit 1
fi
rm "$caller_bin"
if run_build >/dev/null 2>&1; then
  echo "build accepted a missing resolved executable" >&2
  exit 1
fi

mkdir -p "$caller_root/scripts/lib" "$caller_root/actual/release"
cp "$repo_root/scripts/lib/cargo_env.sh" "$caller_root/scripts/lib/cargo_env.sh"
cat > "$mock_bin/cargo" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
[[ "${1:-}" == metadata ]] || exit 1
printf '{"target_directory":"%s"}\n' "${CARGO_TARGET_DIR:?}"
MOCK
cat > "$caller_root/scripts/cargo_with_worktree_build_dir.sh" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${MOCK_BUILD_SKIP:-0}" != 1 ]]; then
  cp "${MOCK_RELEASE_TEMPLATE:?}" "${CARGO_TARGET_DIR:?}/release/harn"
fi
MOCK
chmod +x "$mock_bin/cargo" "$caller_root/scripts/cargo_with_worktree_build_dir.sh"
release_bin="$caller_root/actual/release/harn"
run_release() {
  PATH="$mock_bin:$PATH" HARN_DEV_TARGET_DIR="$caller_root/wrong" \
    CARGO_TARGET_DIR="$caller_root/actual" MOCK_CODESIGN_LOG="$codesign_log" \
    MOCK_RELEASE_TEMPLATE="$target_dir/debug/harn" MOCK_SEAL_MODE=1 \
    HARN_LOCAL_SIGN_QUIET=1 make -s -C "$caller_root" build-release
}
run_release
cmp "$release_bin" "$release_bin.seal"
rm "$release_bin" "$release_bin.seal"
if MOCK_BUILD_SKIP=1 run_release >/dev/null 2>&1; then
  echo "release build accepted a missing executable" >&2
  exit 1
fi
if MOCK_SIGN_DROP=1 run_release >/dev/null 2>&1; then
  echo "release build accepted a signer that did not seal its executable" >&2
  exit 1
fi

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

  # The real build target must refuse an executable changed after signing.
  real_project="$real_root/project"
  mkdir -p "$real_project/scripts" "$real_project/.githooks" "$real_project/target/debug"
  cp "$repo_root/Makefile" "$real_project/Makefile"
  cp "$repo_root/scripts/sign_local_macos.sh" "$real_project/scripts/sign_local_macos.sh"
  cp "$repo_root/.githooks/lib.sh" "$real_project/.githooks/lib.sh"
  cp /usr/bin/true "$real_project/target/debug/harn"
  cat > "$real_project/scripts/harn_bin.sh" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == "--print" ]]; then
  printf '%s\n' "${MAC_RESOLVED_BIN:?}"
elif [[ "${1:-}" == "--record-receipt" && "${MAC_MUTATE_AFTER_SIGN:-0}" == 1 ]]; then
  xcrun vtool -set-build-version macos 14.0 14.0 -replace \
    -output "$MAC_RESOLVED_BIN" "$MAC_RESOLVED_BIN" >/dev/null 2>&1
fi
MOCK
  chmod +x "$real_project/scripts/harn_bin.sh"
  mac_bin="$real_project/target/debug/harn"
  MAC_RESOLVED_BIN="$mac_bin" HARN_LOCAL_SIGN_ID="no such identity" HARN_LOCAL_SIGN_QUIET=1 \
    CARGO_TARGET_DIR="$real_project/target" make -s -C "$real_project" build-harn
  /usr/bin/codesign --verify --strict "$mac_bin"
  if MAC_RESOLVED_BIN="$mac_bin" MAC_MUTATE_AFTER_SIGN=1 HARN_LOCAL_SIGN_ID="no such identity" \
    HARN_LOCAL_SIGN_QUIET=1 CARGO_TARGET_DIR="$real_project/target" \
    make -s -C "$real_project" build-harn >/dev/null 2>&1; then
    echo "build accepted a real signature invalidated after signing" >&2
    exit 1
  fi
  if /usr/bin/codesign --verify --strict "$mac_bin" >/dev/null 2>&1; then
    echo "real mutation did not invalidate the build output" >&2
    exit 1
  fi
fi

echo "local macOS signing fallback tests passed"
