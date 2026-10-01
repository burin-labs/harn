#!/usr/bin/env bash
set -euo pipefail

# One owner for release head-ref recognition; see the header there.
# shellcheck source=.github/scripts/release-ref.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/.github/scripts/release-ref.sh"

usage() {
  cat <<'EOF'
usage: scripts/native_platform_ci_plan.sh --platform macos --event EVENT \
  --changed-files PATH [--head-ref REF] [--ci-diff PATH] \
  [--policy-diff PATH] [--workflow PATH]

Prints true when the changed-file set should run the requested native platform
CI lane. The path policy is intentionally centralized here instead of duplicated
inside ci.yml routing jobs. Native Windows is not a ci.yml lane: it runs in
windows-nightly.yml on its schedule, on dispatch, and on release pull requests.
EOF
}

platform=""
event_name=""
head_ref=""
changed_files=""
ci_diff=""
policy_diff=""
workflow=".github/workflows/ci.yml"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --platform)
      platform="${2:-}"
      shift 2
      ;;
    --event)
      event_name="${2:-}"
      shift 2
      ;;
    --head-ref)
      head_ref="${2:-}"
      shift 2
      ;;
    --changed-files)
      changed_files="${2:-}"
      shift 2
      ;;
    --ci-diff)
      ci_diff="${2:-}"
      shift 2
      ;;
    --policy-diff)
      policy_diff="${2:-}"
      shift 2
      ;;
    --workflow)
      workflow="${2:-}"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unexpected argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ "$platform" != "macos" ]]; then
  echo "error: --platform must be macos" >&2
  exit 2
fi
if [[ -z "$event_name" || -z "$changed_files" ]]; then
  echo "error: --event and --changed-files are required" >&2
  exit 2
fi
if [[ ! -r "$changed_files" ]]; then
  echo "error: changed-files list not readable: $changed_files" >&2
  exit 66
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"

release_metadata_only() {
  local result
  result="$("$repo_root/scripts/ci_release_metadata_only.sh" "$1")"
  [[ "$result" == "true" ]]
}

workflow_ranges() {
  local wanted_csv="$1"
  awk -v wanted_csv="$wanted_csv" '
    BEGIN {
      split(wanted_csv, wanted, ",")
      for (idx in wanted) {
        want[wanted[idx]] = 1
      }
    }
    /^  [A-Za-z0-9_-]+:$/ {
      if (active != "") {
        print active "\t" start "\t" (FNR - 1)
      }
      name = $0
      sub(/^  /, "", name)
      sub(/:$/, "", name)
      if (want[name]) {
        active = name
        start = FNR
      } else {
        active = ""
        start = 0
      }
    }
    END {
      if (active != "") {
        print active "\t" start "\t" FNR
      }
    }
  ' "$workflow"
}

ci_diff_touches_platform() {
  local wanted_csv="$1"
  local ranges_file
  ranges_file="$(mktemp)"
  workflow_ranges "$wanted_csv" > "$ranges_file"

  if [[ ! -s "$ranges_file" || -z "$ci_diff" || ! -r "$ci_diff" || ! -s "$ci_diff" ]]; then
    rm -f "$ranges_file"
    return 0
  fi

  local status
  if awk -v ranges_file="$ranges_file" '
    BEGIN {
      while ((getline row < ranges_file) > 0) {
        split(row, fields, "\t")
        count += 1
        starts[count] = fields[2] + 0
        ends[count] = fields[3] + 0
      }
      close(ranges_file)
    }
    /^@@ / {
      start = 0
      size = 1
      for (idx = 1; idx <= NF; idx += 1) {
        if (substr($idx, 1, 1) == "+") {
          token = substr($idx, 2)
          split(token, parts, ",")
          start = parts[1] + 0
          if (parts[2] != "") {
            size = parts[2] + 0
          }
          break
        }
      }
      if (start <= 0) {
        touched = 1
        next
      }
      end = start
      if (size > 0) {
        end = start + size - 1
      }
      for (range_idx = 1; range_idx <= count; range_idx += 1) {
        if ((end + 1) >= starts[range_idx] && (start - 1) <= ends[range_idx]) {
          touched = 1
        }
      }
    }
    END {
      exit touched ? 0 : 1
    }
  ' "$ci_diff"; then
    status=0
  else
    status=$?
  fi
  rm -f "$ranges_file"
  return "$status"
}

release_control_path() {
  local path="$1"
  [[ "$path" =~ ^(\.github/release-(binary-size-policy\.(jq|json)|runner-policy\.json)|\.github/workflows/(build-release-binaries|release-smoke)\.yml|scripts/release_smoke\.harn|scripts/(release_runner_matrix|smoke_installed_binary)\.sh)$ ]]
}

release_control_diff_mentions_platform() {
  # Release workflows and smoke scripts are mostly control-plane code. A
  # control-plane-only edit should be covered by action hygiene and release
  # script tests instead of paying a hosted native macOS compile. If CI cannot
  # provide the diff, keep the old conservative behavior and run.
  if [[ -z "$policy_diff" || ! -r "$policy_diff" || ! -s "$policy_diff" ]]; then
    return 0
  fi

  grep -Eiq 'macos|Darwin|apple-darwin|x86_64-apple|aarch64-apple|codesign|xcrun|notar' "$policy_diff"
}

path_matches_platform() {
  local path="$1"
  # Keep native source/workflow path policy here, not in ci.yml. The ci.yml file
  # itself is handled separately through hunk/range inspection so unrelated
  # workflow edits do not pay hosted native macOS compiles.
  [[ "$path" =~ ^(Cargo\.lock|Cargo\.toml|rust-toolchain\.toml|\.config/nextest\.toml|crates/harn-modules/src/package_execution(/.*|\.rs)|crates/harn-vm/src/(shells\.rs|stdlib/(process\.rs|sandbox(/.*|\.rs))|vm/tests_runtime(/.*|\.rs))|crates/harn-vm/tests/harn_vm/sandbox_hardened\.rs|crates/harn-hostlib/(src/(secret_store(/.*|\.rs)|tools/proc\.rs)|tests/harn_hostlib/(secret_store_os_native|sandbox_npm_offline_install)\.rs)|crates/harn-terminal/.*|crates/harn-cli/src/(commands/(test|upgrade|doctor|quickstart|hardware|models/install)\.rs|commands/dump_protocol_artifacts/.*|package/manifest\.rs)|spec/protocol-artifacts/HarnProtocol\.swift|\.github/workflows/macos-nightly\.yml)$ ]]
}

if [[ "$event_name" != "push" && "$event_name" != "pull_request" && "$event_name" != "merge_group" ]]; then
  echo false
  exit 0
fi

if { { [[ "$event_name" == "pull_request" ]] && is_release_head_ref "$head_ref"; } \
  || [[ "$event_name" == "push" || "$event_name" == "merge_group" ]]; } \
  && release_metadata_only "$changed_files"; then
  echo false
  exit 0
fi

while IFS= read -r changed_path || [[ -n "$changed_path" ]]; do
  changed_path="${changed_path#./}"
  [[ -z "$changed_path" ]] && continue
  if [[ "$changed_path" == ".github/workflows/ci.yml" ]]; then
    if ci_diff_touches_platform "macos"; then
      echo true
      exit 0
    fi
  elif release_control_path "$changed_path"; then
    if release_control_diff_mentions_platform; then
      echo true
      exit 0
    fi
  elif path_matches_platform "$changed_path"; then
    echo true
    exit 0
  fi
done < "$changed_files"

echo false
