#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
script="$repo_root/scripts/ci/affected_crate_args.sh"

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

global_changes="$tmpdir/global.txt"
cat > "$global_changes" <<'EOF'
.github/workflows/ci.yml
crates/harn-vm/src/lib.rs
EOF

if ! output=$(HARN_BIN="$tmpdir/missing-harn" "$script" --changed-files-file "$global_changes" 2>"$tmpdir/global.err"); then
  cat "$tmpdir/global.err" >&2
  exit 1
fi
if [[ "$output" != "--workspace" ]]; then
  echo "expected global path fast path to select --workspace, got: $output" >&2
  exit 1
fi
grep -q "global/workspace-level change detected" "$tmpdir/global.err"

no_changes="$tmpdir/empty.txt"
: > "$no_changes"
if ! output=$(HARN_BIN="$tmpdir/missing-harn" "$script" --changed-files-file "$no_changes" 2>"$tmpdir/empty.err"); then
  cat "$tmpdir/empty.err" >&2
  exit 1
fi
if [[ -n "$output" ]]; then
  echo "expected empty changed-file set to select nothing, got: $output" >&2
  exit 1
fi
grep -q "no files changed" "$tmpdir/empty.err"

partial_changes="$tmpdir/partial.txt"
echo "crates/harn-vm/src/lib.rs" > "$partial_changes"
set +e
HARN_BIN="$tmpdir/missing-harn" "$script" --changed-files-file "$partial_changes" >"$tmpdir/partial.out" 2>"$tmpdir/partial.err"
status=$?
set -e
if [[ "$status" -eq 0 ]]; then
  echo "expected partial selection to delegate to harn_bin.sh" >&2
  exit 1
fi
grep -q "harn binary is not executable" "$tmpdir/partial.err"

echo "affected_crate_args_test: ok"
