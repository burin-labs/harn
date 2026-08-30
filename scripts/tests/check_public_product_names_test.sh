#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
script="$root/scripts/check_public_product_names.sh"
scanner="$root/scripts/scan_hashed_denylist.mjs"
denylist="$root/scripts/consumer-host-denylist.sha256"

if ! "$script"; then
  echo "error: clean public contract files must pass the scan" >&2
  exit 1
fi

# The real denylist must carry hashes and no plaintext. A hash-shaped line is
# the only payload; anything else is a comment.
if [[ "$(grep -c '^[0-9a-f]\{64\}$' "$denylist")" -lt 1 ]]; then
  echo "error: the infrastructure denylist must contain sha256 entries" >&2
  exit 1
fi
if grep -qiE '^[^#]*[a-z]{4,}\.(local|lan|internal)' "$denylist"; then
  echo "error: the infrastructure denylist must not carry plaintext hostnames" >&2
  exit 1
fi

sha256_of_stdin() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum | awk '{print $1}'
  else
    shasum -a 256 | awk '{print $1}'
  fi
}

fixture_root="$(mktemp -d "${TMPDIR:-/tmp}/harn-public-product-names-fix.XXXXXX")"
trap 'rm -rf "$fixture_root"' EXIT
mkdir -p "$fixture_root/docs" "$fixture_root/scripts"
cp "$script" "$fixture_root/scripts/check_public_product_names.sh"
cp "$scanner" "$fixture_root/scripts/scan_hashed_denylist.mjs"
git -C "$fixture_root" init -q

# The fixture's own denylist covers a benign token throughout, so proving the
# mechanism never requires a real private hostname in this tracked test file.
# That is the rule the gate enforces, applied to the gate's own test.
fixture_token="fixture-denylisted-host.invalid"
printf '%s' "$fixture_token" | sha256_of_stdin >"$fixture_root/scripts/consumer-host-denylist.sha256"

# --- Arm 1: a planted downstream product name must fail ----------------------
product="burin"
printf 'see %s-code#1\n' "$product" >"$fixture_root/docs/public.md"
git -C "$fixture_root" add docs/public.md scripts/
if "$fixture_root/scripts/check_public_product_names.sh"; then
  echo "error: a planted downstream product name must fail the scan" >&2
  exit 1
fi

git -C "$fixture_root" reset -q
rm -f "$fixture_root/docs/public.md"
printf 'historical %s-code#1\n' "$product" >"$fixture_root/CHANGELOG.md"
git -C "$fixture_root" add CHANGELOG.md scripts/
if ! "$fixture_root/scripts/check_public_product_names.sh"; then
  echo "error: immutable changelog history must remain allowlisted" >&2
  exit 1
fi
git -C "$fixture_root" reset -q
rm -f "$fixture_root/CHANGELOG.md"

# --- Arm 2: a planted denylisted token must fail, WITHOUT echoing it ----------
printf 'connect to %s today\n' "$fixture_token" >"$fixture_root/docs/infra.md"
git -C "$fixture_root" add docs/infra.md scripts/

captured="$fixture_root/captured.txt"
if "$fixture_root/scripts/check_public_product_names.sh" >"$captured" 2>&1; then
  echo "error: a planted denylisted token must fail the scan" >&2
  exit 1
fi
if ! grep -q '^docs/infra.md:1: sha256:' "$captured"; then
  echo "error: the scan must report the planted token's file and line" >&2
  cat "$captured" >&2
  exit 1
fi
# The load-bearing assertion: the offending text must not appear in output that
# lands in a public CI log.
if grep -qF "$fixture_token" "$captured"; then
  echo "error: the scan echoed the matched token into its own output" >&2
  exit 1
fi
if grep -qF "fixture-denylisted-host" "$captured"; then
  echo "error: the scan leaked part of the matched token into its output" >&2
  exit 1
fi

# --- Arm 2 control: removing the token clears the scan ------------------------
git -C "$fixture_root" reset -q
rm -f "$fixture_root/docs/infra.md"
git -C "$fixture_root" add scripts/
if ! "$fixture_root/scripts/check_public_product_names.sh"; then
  echo "error: removing the planted token must clear the scan" >&2
  exit 1
fi

echo "check_public_product_names tests passed"
