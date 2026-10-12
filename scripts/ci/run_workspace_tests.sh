#!/usr/bin/env bash
# Census the eligible suite before allowing an empty individual partition.
#
# A partition is `all`, `vm:<k>/3`, `rest:<k>/4`, or `tooling`; the three
# groups together are the workspace. `vm` is harn-vm's own suite (about 44%
# of test time), `tooling` is a few small crates, and `rest` is everything
# else. Each `vm` and `rest` leg compiles only its group, so harn-vm's
# test-mode crate, the largest unit in the graph, no longer competes with its
# library build on every leg. harn-vm keeps all of its targets in one group so
# its feature set is resolved once.
#
# `tooling` belongs to the producer, which has already compiled the whole
# workspace for its security archive. It selects its crates by filter rather
# than by package, because a narrower package selection resolves features
# differently and would rebuild that graph; it is small because the archive
# build already sets the producer's finish time.
set -euo pipefail
tooling_packages=(harn-fmt harn-lsp harn-package)
partition=${1:?partition or all required}
[[ "$partition" =~ ^(all|tooling|vm:[1-3]/3|rest:[1-4]/4)$ ]] || { echo 'Invalid workspace test partition' >&2; exit 1; }
group=${partition%%:*}
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
host_bound_filter=$(bash "$script_dir/host_bound_rust_test_filter.sh")
filter="not (${host_bound_filter})"
if [[ "$group" == tooling ]]; then
  tooling_filter=$(printf ' or package(%s)' "${tooling_packages[@]}")
  filter+=" and (${tooling_filter# or })"
fi
args=(--profile ci -E "$filter")
is_tooling() {
  local name
  for name in "${tooling_packages[@]}"; do [[ "$1" == "$name" ]] && return 0; done
  return 1
}
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
      case "$group" in
        vm) grouped=(-p harn-vm) ;;
        tooling) grouped=(--workspace) ;;
        rest)
          grouped=(--workspace --exclude harn-vm)
          for name in "${tooling_packages[@]}"; do grouped+=(--exclude "$name"); done
          ;;
      esac
    else
      in_group=false
      for ((i = 0; i < ${#packages[@]}; i += 2)); do
        [[ "${packages[$i]}" == -p ]] || { echo "Unexpected package selection: $selected" >&2; exit 1; }
        name=${packages[$((i + 1))]}
        case "$group" in
          vm) [[ "$name" == harn-vm ]] && grouped+=(-p "$name") ;;
          rest) [[ "$name" != harn-vm ]] && ! is_tooling "$name" && grouped+=(-p "$name") ;;
          tooling) is_tooling "$name" && in_group=true ;;
        esac
      done
      # The producer keeps the unified selection; it only needs a member present.
      if [[ "$group" == tooling && "$in_group" == true ]]; then grouped=("${packages[@]}"); fi
    fi
    # Alone, harn-vm resolves only its own default features, which would
    # compile out code that other members enable (harn-cli's `otel`). Build it
    # with the feature set the whole workspace resolves for it.
    if [[ "$group" == vm && ${#grouped[@]} -gt 0 ]]; then
      vm_features=$(cargo metadata --locked --format-version 1 | jq -er '
        (.packages[] | select(.name == "harn-vm" and .source == null) | .id) as $id
        | .resolve.nodes[] | select(.id == $id) | .features | join(",")')
      [[ -n "$vm_features" ]] || { echo 'harn-vm has no resolved workspace features' >&2; exit 1; }
      grouped+=(--features "harn-vm/${vm_features//,/,harn-vm/}")
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
    if [[ "$group" == tooling ]]; then partition=all; else partition="count:${partition#*:}"; fi
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
