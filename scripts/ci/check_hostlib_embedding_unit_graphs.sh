#!/usr/bin/env bash
# Compare the actual Make commands with both CI workspace dependency graphs.
set -euo pipefail
export LC_ALL=C
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
cd "$root"

cat > "$scratch/cargo-graph" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
operation=${1:?Cargo operation required}
case "$operation" in
  build) extra=() ;;
  test) extra=(--no-run) ;;
  *) echo "Unexpected embedding operation: $operation" >&2; exit 1 ;;
esac
RUSTC_BOOTSTRAP=1 cargo "$@" "${extra[@]}" --unit-graph -Z unstable-options \
  > "$(dirname "${BASH_SOURCE[0]}")/$operation.json"
SH
chmod +x "$scratch/cargo-graph"
for platform in Linux Darwin; do
  mkdir "$scratch/$platform"
  cp "$scratch/cargo-graph" "$scratch/$platform/cargo-graph"
  make --no-print-directory test-hostlib-embedding \
    HOSTLIB_EMBEDDING_PLATFORM="$platform" \
    HARN_CARGO_CMD="$scratch/$platform/cargo-graph"
done

diverged=0
for platform in Linux Darwin; do
  selection=(--workspace)
  [[ "$platform" != Darwin ]] || selection+=(--exclude harn-wasm)
  RUSTC_BOOTSTRAP=1 cargo test --locked "${selection[@]}" --no-run \
    --unit-graph -Z unstable-options > "$scratch/writer.json"
  node scripts/ci/compare_hostlib_embedding_unit_graphs.mjs "$platform" \
    "$scratch/writer.json" "$scratch/$platform/build.json" "$scratch/$platform/test.json" || diverged=1
done
exit "$diverged"
