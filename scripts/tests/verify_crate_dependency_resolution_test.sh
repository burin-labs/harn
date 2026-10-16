#!/usr/bin/env bash
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
filter="$root_dir/scripts/verify_crate_dependency_resolution.jq"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/harn-resolution-test.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

cat >"$tmp/metadata.json" <<'JSON'
{
  "packages": [
    {"id": "path+harn-vm", "name": "harn-vm", "version": "1.2.3"},
    {"id": "registry+rmcp", "name": "rmcp", "version": "3.1.4"}
  ],
  "resolve": {
    "nodes": [
      {
        "id": "path+harn-vm",
        "deps": [{"name": "mcp_wire", "pkg": "registry+rmcp"}]
      },
      {"id": "registry+rmcp", "deps": []}
    ]
  }
}
JSON

resolved="$(jq -er \
  --arg package harn-vm \
  --arg package_version 1.2.3 \
  --arg resolution_name mcp_wire \
  -f "$filter" \
  "$tmp/metadata.json")"
if [[ "$resolved" != "3.1.4" ]]; then
  echo "expected package-specific rmcp resolution 3.1.4, got: $resolved" >&2
  exit 1
fi

# Cargo normalizes dependency names (including renamed dependencies) to Rust
# identifiers in resolve.nodes[].deps[].name.
for dependency_name in aws-smithy-types renamed-smithy-types; do
  jq --arg dependency_name "$dependency_name" '
    .packages[1] = {"id":"registry+smithy","name":"aws-smithy-types","version":"1.6.2"}
    | .resolve.nodes[0].deps = [{"name":($dependency_name | gsub("-"; "_")),"pkg":"registry+smithy"}]
    | .resolve.nodes[1].id = "registry+smithy"
  ' "$tmp/metadata.json" >"$tmp/hyphenated.json"
  resolved="$(jq -er --arg package harn-vm --arg package_version 1.2.3 \
    --arg resolution_name "$dependency_name" -f "$filter" "$tmp/hyphenated.json")"
  if [[ "$resolved" != "1.6.2" ]]; then
    echo "hyphenated dependency did not resolve to 1.6.2: $dependency_name" >&2
    exit 1
  fi
done

jq '.packages += [{"id":"path+duplicate","name":"harn-vm","version":"1.2.3"}]' \
  "$tmp/metadata.json" >"$tmp/duplicate.json"
if jq -er \
  --arg package harn-vm \
  --arg package_version 1.2.3 \
  --arg resolution_name mcp_wire \
  -f "$filter" \
  "$tmp/duplicate.json" >"$tmp/duplicate.out" 2>"$tmp/duplicate.err"; then
  echo "duplicate packaged source nodes unexpectedly resolved" >&2
  exit 1
fi
if ! grep -Fq "expected exactly one packaged source node" "$tmp/duplicate.err"; then
  echo "duplicate-node failure did not name the violated invariant" >&2
  cat "$tmp/duplicate.err" >&2
  exit 1
fi

echo "packaged dependency resolution tests passed"

# Exercise the same paired-build owner used by the archive verifier. Cargo is
# replaced at its process boundary so changed graphs and failed builds are
# deterministic; the full publication-path probe supplies the real Cargo proof.
source "$root_dir/scripts/lib/package_verify_bootstrap.sh"
external_publish_pins_table=''
mkdir "$tmp/bin"
cat >"$tmp/bin/cargo" <<'CARGO'
#!/usr/bin/env bash
set -euo pipefail
command=${1:?}
shift
manifest=''
locked=0
workspace=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --manifest-path) manifest=$2; shift 2 ;;
    --locked) locked=1; shift ;;
    --workspace) workspace=1; shift ;;
    *) shift ;;
  esac
done
root=$(dirname "$manifest")
mode=$(cat "$root/mode")
case "$command" in
  generate-lockfile)
    printf latest > "$root/phase"
    if [[ "$mode" == empty_lock ]]; then
      : > "$root/Cargo.lock"
    else
      printf 'latest graph\n' > "$root/Cargo.lock"
    fi
    ;;
  update)
    printf 'update\n' >> "$root/calls"
    if [[ "$mode" == changed || "$mode" == failed_minimum ]]; then
      printf 'minimum graph\n' > "$root/Cargo.lock"
      printf minimum > "$root/phase"
    fi
    ;;
  check)
    [[ "$locked" == 1 && "$workspace" == 1 ]]
    [[ "${HARN_REQUIRE_CLI_AOT:-}" == 1 ]]
    [[ "${CARGO_TARGET_DIR:-}" == "$root/target" ]]
    [[ "${CARGO_BUILD_BUILD_DIR:-}" == "$root/build" ]]
    printf 'check\n' >> "$root/calls"
    if [[ "$mode" == failed_latest ]]; then exit 17; fi
    if [[ "$mode" == failed_minimum && "$(cat "$root/phase")" == minimum ]]; then exit 19; fi
    ;;
  metadata)
    printf 'metadata\n' >> "$root/calls"
    if [[ "$mode" == failed_metadata ]]; then exit 23; fi
    version=3.1.4
    if [[ "$mode" == wrong_floor ]]; then version=3.1.5; fi
    if [[ "$mode" == failed_minimum && "$(cat "$root/phase")" == latest ]]; then version=3.1.5; fi
    if [[ "$mode" == changed && "$(cat "$root/phase")" == minimum ]]; then version=3.1.3; fi
    jq --arg version "$version" '.packages[1].version = $version' "$root/metadata.json"
    ;;
  *) echo "unexpected Cargo command: $command" >&2; exit 1 ;;
esac
CARGO
chmod +x "$tmp/bin/cargo"

resolution_case() {
  local mode=$1 expected_status=$2 expected_checks=$3 expected_metadata=$4
  local fixture="$tmp/$mode" status=0 minimum=3.1.4
  mkdir "$fixture"
  printf '%s\n' "$mode" > "$fixture/mode"
  printf '[workspace]\n' > "$fixture/Cargo.toml"
  cp "$tmp/metadata.json" "$fixture/metadata.json"
  : > "$fixture/calls"
  [[ "$mode" != changed ]] || minimum=3.1.3
  local contract=$'harn-vm\t1.2.3\trmcp\t^3.1\t'
  contract+="$minimum"$'\tmcp_wire'
  if (
    export PATH="$tmp/bin:$PATH"
    package_verify_check_resolutions \
      "$fixture/Cargo.toml" "$fixture/target" "$fixture/build" "$fixture" "$filter" "$contract"
  ) > "$fixture/out" 2> "$fixture/err"; then
    status=0
  else
    status=$?
  fi
  if [[ "$status" != "$expected_status" ]]; then
    echo "resolution case $mode: expected exit $expected_status, got $status" >&2
    cat "$fixture/err" >&2
    exit 1
  fi
  local checks metadata
  checks=$(awk '$0 == "check" { n++ } END { print n+0 }' "$fixture/calls")
  metadata=$(awk '$0 == "metadata" { n++ } END { print n+0 }' "$fixture/calls")
  [[ "$checks" == "$expected_checks" && "$metadata" == "$expected_metadata" ]] || {
    echo "resolution case $mode: checks=$checks metadata=$metadata" >&2
    exit 1
  }
  if [[ "$mode" == unchanged ]]; then
    if grep -Fxq update "$fixture/calls"; then
      echo 'an already satisfied minimum unnecessarily changed dependency resolution' >&2
      exit 1
    fi
    grep -Fq 'Reuse resolver-latest compilation' "$fixture/out"
    grep -Fq 'phase=declared-minimum' "$fixture/out"
  fi
  if [[ "$mode" == changed ]]; then
    grep -Fxq update "$fixture/calls"
    if grep -Fq 'Reuse resolver-latest compilation' "$fixture/out"; then
      echo "changed graph $mode reused a different compilation" >&2
      exit 1
    fi
  fi
}

resolution_case unchanged 0 1 2
resolution_case changed 0 2 2
resolution_case failed_latest 17 1 0
resolution_case failed_minimum 19 2 1
resolution_case failed_metadata 23 1 1
resolution_case wrong_floor 1 1 2
resolution_case empty_lock 1 1 1
echo "paired package builds: identical and changed graphs, failed builds, failed metadata, wrong floor, and empty lock controls passed"
