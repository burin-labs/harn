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

# Non-Rust crate assets may be embedded with include_str!/include_bytes! and
# therefore participate in executable freshness just like .rs files.
freshness_repo="$tmp_root/freshness-repo"
mkdir -p "$freshness_repo/crates/example/src/explanations"
git -C "$freshness_repo" init -q
printf '# embedded explanation\n' > "$freshness_repo/crates/example/src/explanations/example.md"
git -C "$freshness_repo" add crates/example/src/explanations/example.md

embedded_bin="$tmp_root/embedded-harn"
cp "$fake_bin" "$embedded_bin"
touch -t 202001010000 "$embedded_bin"
touch -t 202101010000 "$freshness_repo/crates/example/src/explanations/example.md"

if (
  cd "$freshness_repo"
  source "$repo_root/scripts/lib/harn_bin.sh"
  harn_bin_newer_source_report "$embedded_bin"
) > "$tmp_root/embedded-stale.out"; then
  echo "harn_bin resolver accepted a binary older than an embedded crate asset" >&2
  exit 1
fi
if ! grep -Fxq "crates/example/src/explanations/example.md" "$tmp_root/embedded-stale.out"; then
  echo "harn_bin resolver did not report the newer embedded crate asset" >&2
  cat "$tmp_root/embedded-stale.out" >&2
  exit 1
fi

echo "harn_bin_resolver_test: ok"
