#!/usr/bin/env bash
set -euo pipefail

target="$(realpath "${1:?target directory required}")"
report="${2:?report path required}"
workspace="$(pwd -P)"
[[ "$target" == "$workspace/target" ]] || { echo 'archive measurement requires this workspace target' >&2; exit 1; }
native="$(find "$target/debug/deps" -maxdepth 1 -type f -perm -u+x ! -name '*.*' -name 'harn_vm-*' | wc -l | tr -d ' ')"
[[ "$native" =~ ^[1-9][0-9]*$ ]] || { echo 'archive measurement did not observe Native executables' >&2; exit 1; }
target_kib="$(du -sk "$target" | cut -f1)"
available_kib="$(df -Pk "${RUNNER_TEMP:-/tmp}" | awk 'NR == 2 {print $4}')"
[[ "$target_kib" =~ ^[1-9][0-9]*$ && "$available_kib" =~ ^[1-9][0-9]*$ ]] || { echo 'archive disk capacity is unmeasured' >&2; exit 1; }
((available_kib > target_kib + 1048576)) || { echo 'archive measurement has insufficient private scratch space' >&2; exit 1; }

scratch="$(mktemp -d "${RUNNER_TEMP:-/tmp}/harn-workspace-archive.XXXXXXXX")"
trap 'rm -rf -- "$scratch"' EXIT
archive="$scratch/target.tar.zst"
mkdir "$scratch/extracted"
# This qualification measures the target payload. Swatinem separately removes
# top-level outputs and adds Cargo registries before its actual cache save.
/usr/bin/time -q -f '%e' -o "$scratch/compress.seconds" \
  tar -C "$target" --use-compress-program 'zstd -T2' -cf "$archive" .
compressed_bytes="$(stat -c '%s' "$archive")"
available_kib="$(df -Pk "$scratch" | awk 'NR == 2 {print $4}')"
[[ "$available_kib" =~ ^[1-9][0-9]*$ ]] || { echo 'extraction disk capacity is unmeasured' >&2; exit 1; }
((available_kib > target_kib + 1048576)) || { echo 'archive extraction has insufficient private scratch space' >&2; exit 1; }
/usr/bin/time -q -f '%e' -o "$scratch/extract.seconds" \
  tar -C "$scratch/extracted" --use-compress-program unzstd -xf "$archive"
# Compare actual extracted bytes and metadata against the archive, rather than
# accepting successful extraction or a same-sized file inventory as proof.
tar -C "$scratch/extracted" --use-compress-program unzstd -df "$archive"
read -r compress_seconds < "$scratch/compress.seconds"
read -r extract_seconds < "$scratch/extract.seconds"
node - "$report" "$target_kib" "$compressed_bytes" "$compress_seconds" "$extract_seconds" "$native" <<'NODE'
const fs = require('node:fs');
const [report,kib,compressed,compress,extract,native] = process.argv.slice(2);
const values = [kib,compressed,compress,extract,native].map(Number);
if (!values.every(v => Number.isFinite(v) && v >= 0) || values[0] <= 0 || values[1] <= 0 || values[4] <= 0) {
  throw new Error('Archive measurement has invalid numeric evidence');
}
const evidence = {schema:'harn.workspace_target_archive_measurement.v1',
  source:process.env.GITHUB_SHA || null,target_disk_bytes:values[0]*1024,
  compressed_bytes:values[1],compression_seconds:values[2],extraction_seconds:values[3],
  native_executables:values[4],extracted_payload_compared:true,pending:0,bad:0,
  scope:'Workspace target only; not an exact Swatinem cache or a qualifying full CI SLO run.'};
fs.writeFileSync(report,JSON.stringify(evidence,null,2)+'\n');
console.log(JSON.stringify(evidence));
NODE
