#!/usr/bin/env bash
# Census the eligible suite before allowing an empty individual partition.
#
# Native and CLI unit shares build only the library test harness. Separate
# shares build integration and binary targets, which need the ordinary library.
# The remaining packages do not compile either large package's test harnesses.
# All narrowed groups retain the workspace-resolved feature set.
#
# `tooling` runs independently of the security producer and compiles its share.
# Every narrowed package selection explicitly preserves workspace features.
set -euo pipefail
tooling_packages=(harn-fmt harn-lsp harn-package)
partition=${1:?partition or all required}
[[ "$partition" =~ ^(all|tooling|vm-unit:[1-3]/3|vm-integration|cli-unit|cli-integration|rest:[1-4]/4)$ ]] || { echo 'Invalid workspace test partition' >&2; exit 1; }
group=${partition%%:*}
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
harn=(env "HARN_EXT_BOOTSTRAP_INSTALLER=${HARN_BIN:-${HARN_EXT_BOOTSTRAP_INSTALLER:-}}"
  "$script_dir/../bootstrap-harn.sh" --seed-command run --standalone --no-sandbox)
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
        vm-unit|vm-integration) grouped=(-p harn-vm) ;;
        cli-unit|cli-integration) grouped=(-p harn-cli) ;;
        tooling)
          for name in "${tooling_packages[@]}"; do grouped+=(-p "$name"); done
          ;;
        rest)
          grouped=(--workspace --exclude harn-vm --exclude harn-cli)
          for name in "${tooling_packages[@]}"; do grouped+=(--exclude "$name"); done
          ;;
      esac
    else
      for ((i = 0; i < ${#packages[@]}; i += 2)); do
        [[ "${packages[$i]}" == -p ]] || { echo "Unexpected package selection: $selected" >&2; exit 1; }
        name=${packages[$((i + 1))]}
        case "$group" in
          vm-unit|vm-integration) [[ "$name" == harn-vm ]] && grouped+=(-p "$name") ;;
          cli-unit|cli-integration) [[ "$name" == harn-cli ]] && grouped+=(-p "$name") ;;
          rest) [[ "$name" != harn-vm && "$name" != harn-cli ]] && ! is_tooling "$name" && grouped+=(-p "$name") ;;
          tooling) is_tooling "$name" && grouped+=(-p "$name") ;;
        esac
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
    if [[ "$group" == rest && "$selected" == --workspace ]]; then
      feature_packages=(--workspace-except harn-vm harn-cli "${tooling_packages[@]}")
    else
      feature_packages=()
      for ((i = 1; i < ${#grouped[@]}; i += 2)); do feature_packages+=("${grouped[$i]}"); done
      # The CLI directly depends on Native; preserve Native's workspace feature
      # selection as well as the CLI's own features.
      [[ "$group" != cli-* ]] || feature_packages+=(harn-vm)
    fi
    features=$(bash "$script_dir/workspace_features.sh" "${feature_packages[@]}")
    [[ -z "$features" ]] || grouped+=(--features "$features")
    case "$group" in
      vm-unit|cli-unit) grouped+=(--lib) ;;
      # Cargo expands the quoted pattern, including future integration targets.
      # --tests would also select the library unit harness and duplicate it.
      vm-integration|cli-integration) grouped+=(--test '*' --bins) ;;
    esac
    packages=("${grouped[@]}")
    selected="${packages[*]}"
    if [[ "$group" == tooling || "$group" == vm-integration || "$group" == cli-* ]]; then partition=all; else partition="count:${partition#*:}"; fi
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
  build_evidence=
  build_options=()
  if [[ "${GITHUB_ACTIONS:-}" == true && "${GITHUB_WORKSPACE:-}" == "$(pwd -P)" && -n "${RUNNER_TEMP:-}" \
      && "$group" == rest && "$partition" =~ ^count:[12]/4$ ]]; then
    build_evidence="$RUNNER_TEMP/workspace-build-evidence"
    mkdir "$build_evidence"
    [[ -z "${GITHUB_OUTPUT:-}" ]] || echo 'workspace-build-evidence=true' >> "$GITHUB_OUTPUT"
    "${harn[@]}" "$script_dir/workspace_build_evidence.harn" -- snapshot "$GITHUB_WORKSPACE/target" "$build_evidence/before.json"
    build_options=(--timings)
  fi
  cargo-nextest nextest list "${args[@]}" "${build_options[@]}" --message-format json > "$census"
  if [[ -n "$build_evidence" ]]; then
    "${harn[@]}" "$script_dir/workspace_build_evidence.harn" -- report "$GITHUB_WORKSPACE/target" \
      "$build_evidence/report.json" "$build_evidence/before.json" "$census"
    cp "$GITHUB_WORKSPACE/target/cargo-timings/cargo-timing.html" "$build_evidence/compile-timings.html"
  fi
fi
if [[ -n "${RUNNER_TEMP:-}" ]]; then
  cp "$census" "$RUNNER_TEMP/harn-nextest-inventory.json"
fi
eligible=$("${harn[@]}" "$script_dir/nextest_inventory.harn" -- "$census")
echo "Workspace tests: packages=$selected eligible=$eligible partition=$partition"
if [[ "$partition" == all ]]; then
  cargo-nextest nextest run "${args[@]}" --no-tests fail
else
  cargo-nextest nextest run "${args[@]}" --partition "$partition" --no-tests pass
fi
