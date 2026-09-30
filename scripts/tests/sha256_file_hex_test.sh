#!/usr/bin/env bash
# The shell file digest has one owner, scripts/lib/sha256.sh, and it returns
# only the 64-hex digest on every OS.
#
# A Windows release candidate was refused because `sha256sum PATH` prints
# `\<hex>  D:\\...` when PATH holds a backslash: GNU's escape marker. The
# scripts took the first field, kept the marker, and compared it to the same
# digest without one.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

fail() {
  echo "sha256_file_hex_test: $*" >&2
  exit 1
}

# shellcheck source=scripts/lib/sha256.sh
source "$root/scripts/lib/sha256.sh"

printf 'release archive bytes\n' > "$tmp/archive.zip"
real="$(sha256_file_hex "$tmp/archive.zip")"
[[ "$real" =~ ^[0-9a-f]{64}$ ]] || fail "real digest is not 64-hex: $real"

# A sha256sum that behaves like GNU coreutils on a Windows runner: given a file
# name with a backslash it escapes the line; given standard input it does not.
fake_bin="$tmp/bin"
mkdir -p "$fake_bin"
cat > "$fake_bin/sha256sum" <<EOF
#!/usr/bin/env bash
if [[ \$# -gt 0 ]]; then
  printf '\\\\%s  D:\\\\\\\\a\\\\\\\\_temp\\\\\\\\%s\\n' "$real" "\$(basename "\$1")"
else
  cat > /dev/null
  printf '%s  -\\n' "$real"
fi
EOF
chmod +x "$fake_bin/sha256sum"
escaped_line="$(PATH="$fake_bin:$PATH" sha256sum "$tmp/archive.zip")"
[[ "$escaped_line" == "\\$real  "* ]] \
  || fail "the fake does not reproduce the escaped Windows line: $escaped_line"

digest="$(PATH="$fake_bin:$PATH" sha256_file_hex "$tmp/archive.zip")"
[[ "$digest" == "$real" ]] || fail "digest under the escaping sha256sum is '$digest', not $real"

# Every copy that used to take the first field of `sha256sum PATH` now answers
# the same digest under the escaping sha256sum.
# shellcheck source=scripts/lib/candidate_archive_contract.sh
source "$root/scripts/lib/candidate_archive_contract.sh"
[[ "$(PATH="$fake_bin:$PATH" sha256_file "$tmp/archive.zip")" == "$real" ]] \
  || fail "candidate_archive_contract.sh sha256_file keeps the escape marker"

# Anything that is not a digest is refused, not passed through.
cat > "$fake_bin/sha256sum" <<'EOF'
#!/usr/bin/env bash
cat > /dev/null
printf 'garbage  -\n'
EOF
if PATH="$fake_bin:$PATH" sha256_file_hex "$tmp/archive.zip" > "$tmp/garbage.out" 2>&1; then
  fail "a non-digest was accepted: $(cat "$tmp/garbage.out")"
fi
grep -Fq "not a 64-hex digest" "$tmp/garbage.out" || fail "the refusal does not say why: $(cat "$tmp/garbage.out")"
if sha256_file_hex "$tmp/missing.zip" 2>/dev/null; then
  fail "a missing file was hashed"
fi

# Census: no script or workflow hashes a file by name and parses the line.
# The two exemptions print "hash  name" listings that are hashed again as a
# fingerprint; they never compare a digest.
hits="$(cd "$root" && git grep -n -E '(sha256sum|shasum -a 256) +"\$' -- scripts .github \
  ':!scripts/tests' ':!scripts/lib/sha256.sh' || true)"
unexpected="$(grep -v -E '^scripts/(dev_setup\.sh|claude-dev-setup-once\.sh):' <<<"$hits" || true)"
if [[ -n "$unexpected" ]]; then
  echo "sha256_file_hex_test: these hash a file by name; use sha256_file_hex from scripts/lib/sha256.sh:" >&2
  echo "$unexpected" >&2
  exit 1
fi

echo "sha256_file_hex_test: ok"
