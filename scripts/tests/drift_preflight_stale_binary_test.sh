#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
real_harn=${HARN_BIN:?drift_preflight_stale_binary_test requires HARN_BIN}

tmp_root=$(mktemp -d)
mirror="docs/src/language-spec.md"
mirror_path="$repo_root/$mirror"
mirror_backup="$tmp_root/language-spec.md"
cp -p "$mirror_path" "$mirror_backup"

cleanup() {
  local status=$?
  cp -p "$mirror_backup" "$mirror_path"
  rm -rf "$tmp_root"
  exit "$status"
}
trap cleanup EXIT

fake_bin="$tmp_root/bin"
mkdir -p "$fake_bin"

cat > "$fake_bin/harn" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
case "${1:-}" in
  check|lint|parse)
    printf 'forbidden stale parser command: %s\n' "$*" >> "${STALE_HARN_RECORD:?}"
    exit 97
    ;;
esac
exec "${STALE_HARN_REAL:?}" "$@"
SH
chmod +x "$fake_bin/harn"

cat > "$fake_bin/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'forbidden cargo command: %s\n' "$*" >> "${STALE_HARN_RECORD:?}"
exit 98
SH
chmod +x "$fake_bin/cargo"

missing_target="$tmp_root/missing-target"
no_build_output="$tmp_root/no-build.out"
if env -u HARN_BIN \
  CARGO_TARGET_DIR="$missing_target" \
  PATH="$fake_bin:$PATH" \
  STALE_HARN_RECORD="$tmp_root/no-build-cargo.txt" \
  make -C "$repo_root" --no-print-directory check-drift > "$no_build_output" 2>&1; then
  echo "source drift preflight built or accepted a missing worktree interpreter" >&2
  cat "$no_build_output" >&2
  exit 1
fi
if ! grep -Fq "no fresh worktree harn binary found" "$no_build_output"; then
  echo "source drift preflight missing-binary failure was not attributable" >&2
  cat "$no_build_output" >&2
  exit 1
fi
if [[ -e "$tmp_root/no-build-cargo.txt" ]]; then
  echo "source drift preflight invoked Cargo while resolving its interpreter" >&2
  cat "$tmp_root/no-build-cargo.txt" >&2
  exit 1
fi

printf '\n<!-- stale-binary source-preflight negative -->\n' >> "$mirror_path"
record="$tmp_root/forbidden-commands.txt"
output="$tmp_root/check-drift.out"

if PATH="$fake_bin:$PATH" \
  HARN_BIN="$fake_bin/harn" \
  STALE_HARN_REAL="$real_harn" \
  STALE_HARN_RECORD="$record" \
  make -C "$repo_root" --no-print-directory check-drift > "$output" 2>&1; then
  echo "source drift preflight accepted a stale committed mirror" >&2
  cat "$output" >&2
  exit 1
fi

if ! grep -Fq "error: $mirror is stale relative to spec/chapters/*.md" "$output"; then
  echo "source drift preflight failure did not identify the stale mirror" >&2
  cat "$output" >&2
  exit 1
fi
if [[ -e "$record" ]]; then
  echo "source drift preflight invoked stale parser bytes or Cargo" >&2
  cat "$record" >&2
  exit 1
fi

echo "drift_preflight_stale_binary_test: ok"
