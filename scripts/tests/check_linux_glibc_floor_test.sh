#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

touch "$tmp_dir/harn"
cat >"$tmp_dir/readelf" <<'EOF'
#!/usr/bin/env bash
cat "$HARN_TEST_READELF_OUTPUT"
EOF
chmod +x "$tmp_dir/readelf"

cat >"$tmp_dir/compatible.txt" <<'EOF'
  0x0010: Name: GLIBC_2.17  Flags: none  Version: 10
  0x0020: Name: GLIBC_2.34  Flags: none  Version: 9
  0x0030: Name: GLIBC_2.35  Flags: none  Version: 8
EOF
HARN_READELF_BIN="$tmp_dir/readelf" \
  HARN_TEST_READELF_OUTPUT="$tmp_dir/compatible.txt" \
  "$repo_root/scripts/check_linux_glibc_floor.sh" "$tmp_dir/harn" 2.35

cat >"$tmp_dir/too-new.txt" <<'EOF'
  0x0010: Name: GLIBC_2.39  Flags: none  Parent: GLIBC_2.17
EOF
if HARN_READELF_BIN="$tmp_dir/readelf" \
  HARN_TEST_READELF_OUTPUT="$tmp_dir/too-new.txt" \
  "$repo_root/scripts/check_linux_glibc_floor.sh" "$tmp_dir/harn" 2.35; then
  echo "expected a binary above the GLIBC floor to fail" >&2
  exit 1
fi

cat >"$tmp_dir/readelf-fails" <<'EOF'
#!/usr/bin/env bash
exit 17
EOF
chmod +x "$tmp_dir/readelf-fails"
if HARN_READELF_BIN="$tmp_dir/readelf-fails" \
  "$repo_root/scripts/check_linux_glibc_floor.sh" "$tmp_dir/harn" 2.35; then
  echo "expected readelf failure to fail closed" >&2
  exit 1
fi

echo "linux glibc floor check: ok"
