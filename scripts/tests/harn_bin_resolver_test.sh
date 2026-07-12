#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)

tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

fake_bin="$tmp_root/harn"
cat > "$fake_bin" <<'SH'
#!/usr/bin/env bash
printf 'fake harn\n'
SH
chmod +x "$fake_bin"
touch -t 200001010000 "$fake_bin"

if HARN_BIN="$fake_bin" "$repo_root/scripts/harn_bin.sh" --print >"$tmp_root/stale.out" 2>"$tmp_root/stale.err"; then
  echo "harn_bin resolver accepted a stale explicit HARN_BIN" >&2
  cat "$tmp_root/stale.out" >&2
  exit 1
fi
if ! grep -Fq "harn binary is stale" "$tmp_root/stale.err"; then
  echo "stale HARN_BIN error did not explain the freshness failure" >&2
  cat "$tmp_root/stale.err" >&2
  exit 1
fi

HARN_BIN="$fake_bin" \
  HARN_BIN_ASSUME_FRESH=1 \
  "$repo_root/scripts/harn_bin.sh" --print >"$tmp_root/fresh.out"
if ! grep -Fxq "$fake_bin" "$tmp_root/fresh.out"; then
  echo "harn_bin resolver did not return the explicit assumed-fresh binary" >&2
  cat "$tmp_root/fresh.out" >&2
  exit 1
fi

echo "harn_bin_resolver_test: ok"
