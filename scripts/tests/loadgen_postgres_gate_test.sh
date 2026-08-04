#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)

tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

fake_bin="$tmp_root/bin"
record="$tmp_root/cargo-record.txt"
mkdir -p "$fake_bin"

cat > "$fake_bin/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'args=%s\n' "$*" >> "${FAKE_CARGO_RECORD:?}"
printf 'url=%s\n' "${HARN_TEST_POSTGRES_URL-__unset__}" >> "$FAKE_CARGO_RECORD"
SH
chmod +x "$fake_bin/cargo"

env -u HARN_TEST_POSTGRES_URL \
  PATH="$fake_bin:$PATH" \
  make --no-print-directory -C "$repo_root" loadgen-postgres \
  > "$tmp_root/unset.out"

if [[ -e "$record" ]]; then
  echo "loadgen-postgres invoked Cargo with HARN_TEST_POSTGRES_URL unset" >&2
  cat "$record" >&2
  exit 1
fi
if ! grep -Fq "HARN_TEST_POSTGRES_URL not set" "$tmp_root/unset.out"; then
  echo "loadgen-postgres did not explain the unset-URL skip" >&2
  cat "$tmp_root/unset.out" >&2
  exit 1
fi

HARN_TEST_POSTGRES_URL="" \
  PATH="$fake_bin:$PATH" \
  make --no-print-directory -C "$repo_root" loadgen-postgres \
  > "$tmp_root/empty.out"

if [[ -e "$record" ]]; then
  echo "loadgen-postgres invoked Cargo with HARN_TEST_POSTGRES_URL empty" >&2
  cat "$record" >&2
  exit 1
fi

HARN_TEST_POSTGRES_URL="postgres://localhost/harn_loadgen" \
  FAKE_CARGO_RECORD="$record" \
  PATH="$fake_bin:$PATH" \
  make --no-print-directory -C "$repo_root" loadgen-postgres

if ! grep -Fxq \
  "args=run --release -p harn-postgres-perf --bin harn-postgres-loadgen" \
  "$record"; then
  echo "loadgen-postgres did not invoke the expected Cargo command when configured" >&2
  cat "$record" >&2
  exit 1
fi
if ! grep -Fxq "url=postgres://localhost/harn_loadgen" "$record"; then
  echo "loadgen-postgres did not preserve HARN_TEST_POSTGRES_URL" >&2
  cat "$record" >&2
  exit 1
fi

echo "loadgen_postgres_gate_test: ok"
