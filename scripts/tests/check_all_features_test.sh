#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

fake_bin="$tmp/bin"
record="$tmp/cargo-record.txt"
mkdir -p "$fake_bin"

cat >"$fake_bin/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'ORT_SKIP_DOWNLOAD=%s args=%s\n' "${ORT_SKIP_DOWNLOAD-__unset__}" "$*" >>"$FAKE_CARGO_RECORD"
SH
chmod +x "$fake_bin/cargo"

FAKE_CARGO_RECORD="$record" \
  HARN_CARGO_CMD="$fake_bin/cargo" \
  "$repo_root/scripts/check_all_features.sh"

if ! grep -Fxq "ORT_SKIP_DOWNLOAD=1 args=check --locked --workspace --all-features --exclude harn-hostlib" "$record"; then
  echo "missing workspace all-features invocation:" >&2
  cat "$record" >&2
  exit 1
fi
if ! grep -Fxq "ORT_SKIP_DOWNLOAD=1 args=check --locked -p harn-hostlib --features full,computer" "$record"; then
  echo "missing harn-hostlib feature invocation:" >&2
  cat "$record" >&2
  exit 1
fi

echo "check_all_features_test: ok"
