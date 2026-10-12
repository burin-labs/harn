#!/usr/bin/env bash
# Census the eligible suite before allowing an empty individual partition.
#
# A partition is `all` or `<group>:<k>/3`. The `vm` group is harn-vm's own
# suite and `rest` is every other package; together they are the workspace.
# Each group is compiled alone, so a leg never builds the other group's test
# targets: harn-vm's test-mode crate is the largest unit in the graph and
# otherwise competes with its library build on every leg. harn-vm keeps all of
# its targets in one group so its feature set is resolved once.
set -euo pipefail
partition=${1:?partition or all required}
[[ "$partition" == all || "$partition" =~ ^(vm|rest):[1-3]/3$ ]] || { echo 'Invalid workspace test partition' >&2; exit 1; }
group=${partition%%:*}
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
host_bound_filter=$(bash "$script_dir/host_bound_rust_test_filter.sh")
args=(--profile ci -E "not (${host_bound_filter})")
extracted=
if [[ -n "${2:-}" ]]; then
  [[ "$partition" == all ]] || { echo 'Archive mode runs the whole suite' >&2; exit 1; }
  selected=archive
  args+=(--archive-file "$2" --workspace-remap "${GITHUB_WORKSPACE:?workspace remap required}")
else
  selected=$(bash "$script_dir/workspace_test_args.sh")
  read -ra packages <<< "$selected"
  if [[ "$group" != all ]]; then
    grouped=()
    if [[ "$selected" == --workspace ]]; then
      if [[ "$group" == vm ]]; then grouped=(-p harn-vm); else grouped=(--workspace --exclude harn-vm); fi
    else
      for ((i = 0; i < ${#packages[@]}; i += 2)); do
        [[ "${packages[$i]}" == -p ]] || { echo "Unexpected package selection: $selected" >&2; exit 1; }
        name=${packages[$((i + 1))]}
        if [[ "$group" == vm && "$name" == harn-vm ]] || [[ "$group" == rest && "$name" != harn-vm ]]; then
          grouped+=(-p "$name")
        fi
      done
    fi
    if ((${#grouped[@]} == 0)); then
      echo "Workspace tests: packages=$selected group=$group selects nothing; partition=$partition skipped"
      if [[ -n "${RUNNER_TEMP:-}" ]]; then
        jq -n --arg selected "$selected" --arg partition "$partition" \
          '{skipped: {reason: "group selects no affected package", selected: $selected, partition: $partition}}' \
          > "$RUNNER_TEMP/harn-nextest-inventory.json"
      fi
      exit 0
    fi
    packages=("${grouped[@]}")
    selected="${packages[*]}"
    partition="count:${partition#*:}"
  fi
  args+=(--locked "${packages[@]}")
fi
census=$(mktemp)
trap 'rm -f "$census"; if [[ -n "$extracted" ]]; then rm -rf "$extracted"; fi' EXIT
if [[ "$selected" == archive ]]; then
  extracted=$(mktemp -d)
  cargo-nextest nextest list "${args[@]}" --extract-to "$extracted" --message-format json > "$census"
  # Reuse nextest's normalized metadata rather than unpacking the archive twice.
  args=(--profile ci -E "not (${host_bound_filter})"
    --cargo-metadata "$extracted/target/nextest/cargo-metadata.json"
    --binaries-metadata "$extracted/target/nextest/binaries-metadata.json"
    --target-dir-remap "$extracted/target" --workspace-remap "$GITHUB_WORKSPACE")
else
  cargo-nextest nextest list "${args[@]}" --message-format json > "$census"
fi
if [[ -n "${RUNNER_TEMP:-}" ]]; then
  cp "$census" "$RUNNER_TEMP/harn-nextest-inventory.json"
fi
eligible=$(node "$script_dir/nextest_inventory.mjs" "$census")
echo "Workspace tests: packages=$selected eligible=$eligible partition=$partition"
if [[ "$partition" == all ]]; then
  cargo-nextest nextest run "${args[@]}" --no-tests fail
else
  cargo-nextest nextest run "${args[@]}" --partition "$partition" --no-tests pass
fi
