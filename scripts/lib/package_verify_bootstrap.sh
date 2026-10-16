#!/usr/bin/env bash

# Shared bootstrap for package verification. Cargo selects both packages so
# their dependency features agree. Generate missing AOT inputs before the
# canonical resolver builds and certifies Harn against them.
#
# The caller passes a directory it owns for the lifetime of verification. The
# AOT generator executes from a snapshot taken into it rather than from the
# shared Cargo target directory, so a concurrent build cannot relink the
# executable this helper is in the middle of running. Harn itself reaches the
# same boundary through the stable copy release orchestration supplies.

# Frozen packaging still resolves the workspace's registry dependencies. A
# supplied toolset has not populated this job's Cargo home as a local tool
# build would, so fetch the locked host graph before entering that boundary.
package_verify_fetch_dependencies() {
  local identity host
  identity="$(rustc -vV)" || return $?
  host="$(sed -n 's/^host: //p' <<<"$identity")" || return $?
  if [[ -z "$host" ]]; then
    echo 'error: rustc did not report a host for locked package dependency fetch' >&2
    return 1
  fi
  cargo fetch --locked --target "$host" || return $?
}

package_verify_prepare_tools() {
  local root_dir="$1"
  local tool_dir="${2:-}"
  local aot_generator
  local explicit_harn="${HARN_BIN:-}"
  local supplied_generator="${HARN_RELEASE_CLI_AOT_GEN_BIN:-}"
  local generated_dir="$root_dir/crates/harn-cli/generated"
  local no_build="${HARN_BIN_NO_BUILD:-0}"
  local resolved
  local target_dir
  local generate_payload=0
  local generator_snapshot=""

  if [[ -z "$tool_dir" ]]; then
    echo "error: package_verify_prepare_tools requires a caller-owned tool directory" >&2
    return 1
  fi

  case "$no_build" in
  0 | 1) ;;
  *)
    echo "error: HARN_BIN_NO_BUILD must be 0 or 1, got: $no_build" >&2
    return 1
    ;;
  esac

  # shellcheck source=scripts/lib/cargo_env.sh
  source "$root_dir/scripts/lib/cargo_env.sh"
  # shellcheck source=scripts/lib/harn_bin.sh
  source "$root_dir/scripts/lib/harn_bin.sh"
  target_dir="${CARGO_TARGET_DIR:-}"
  if [[ -z "$target_dir" ]]; then
    target_dir="$(cd "$root_dir" && harn_cargo_metadata_target_dir)"
  fi
  aot_generator="$supplied_generator"
  if [[ -z "$aot_generator" ]]; then
    aot_generator="$(CARGO_TARGET_DIR="$target_dir" harn_debug_named_binary_path harn-cli-aot-gen)"
  fi
  if [[ ! -f "$generated_dir/cli-bytecode-manifest.json" || \
        ! -d "$generated_dir/cli-bytecode" ]]; then
    generate_payload=1
  fi

  if [[ -n "$supplied_generator" ]]; then
    harn_require_executable_bin "$supplied_generator" || return $?
  elif [[ "$no_build" == "1" ]]; then
    if [[ ! -x "$aot_generator" ]]; then
      echo "error: HARN_BIN_NO_BUILD=1 but the package verification AOT generator is not built: $aot_generator" >&2
      return 1
    fi
  elif [[ -n "$explicit_harn" ]]; then
    # Release orchestration supplies a stable exact-tree Harn copy so parallel
    # Rust builds cannot relink the executable while package scripts use it.
    # Preserve that boundary and build only the generator it does not supply.
    (cd "$root_dir" && "$root_dir/scripts/cargo_with_worktree_build_dir.sh" build \
      -p harn-cli-aot-gen --bin harn-cli-aot-gen)
  elif [[ "$generate_payload" == "1" ]]; then
    # Select CLI features too, but build only the generator until its outputs
    # exist. Compiling Harn now would embed an incomplete AOT input inventory.
    (cd "$root_dir" && "$root_dir/scripts/cargo_with_worktree_build_dir.sh" build \
      -p harn-cli \
      -p harn-cli-aot-gen --bin harn-cli-aot-gen)
  fi
  if [[ "$generate_payload" == "1" ]]; then
    generator_snapshot="$(harn_snapshot_binary "$aot_generator" "$tool_dir" harn-cli-aot-gen-bootstrap)" || return $?
    "$generator_snapshot" --workspace-root "$root_dir"
  fi
  if [[ -n "$explicit_harn" ]]; then
    resolved="$(cd "$root_dir" && HARN_BIN="$explicit_harn" HARN_BIN_NO_BUILD=1 \
      "$root_dir/scripts/harn_bin.sh" --print)"
  elif [[ "$no_build" == "1" ]]; then
    resolved="$(cd "$root_dir" && HARN_BIN='' HARN_BIN_NO_BUILD=1 \
      "$root_dir/scripts/harn_bin.sh" --print)"
  else
    # One resolver owns the feature-unified build and the final provenance.
    # Repeated preparation must not overwrite its certified binary with a
    # second Cargo invocation that drops the checker feature and freshness ID.
    resolved="$(cd "$root_dir" && HARN_BIN='' HARN_BIN_NO_BUILD=0 \
      "$root_dir/scripts/harn_bin.sh" --print --package-tools)"
  fi
  if [[ ! -x "$resolved" ]]; then
    echo "error: package verification Harn binary is not executable: $resolved" >&2
    return 1
  fi
  if [[ ! -x "$aot_generator" ]]; then
    echo "error: package verification AOT generator is not executable: $aot_generator" >&2
    return 1
  fi
  HARN_BIN="$resolved"
  export HARN_BIN
  # Take the generator out of the shared target directory before running it.
  # Package verification outlives the build that produced this executable, and
  # a parallel Cargo invocation replacing it mid-run kills the process rather
  # than failing a check.
  generator_snapshot="$(harn_snapshot_binary "$aot_generator" "$tool_dir" harn-cli-aot-gen)" || return $?
  # release_gate.sh already generated before parallel readers started. A
  # complete payload is read-only here, including after the resolver returns.
  "$generator_snapshot" --workspace-root "$root_dir" --check
}

# Resolver-latest has no lockfile. A known broken external release may be
# pinned temporarily, but every pin has a review date after which it stops
# changing resolution automatically. The date and reason are part of the
# machine-read table, not a comment that a gate cannot enforce.
# Fields: crate | last good version | expiry (UTC) | broken release and cause.
external_publish_pins_table='
tinyvec|1.12.0|2026-10-03|tinyvec 1.13.0 published 2026-09-02 does not compile because vec! is unreachable in tinyvec.rs
'

apply_external_publish_pins() {
  local manifest="$1"
  local today="${2:-$(date -u +%F)}"
  local crate precise_version expires_on reason
  local LC_COLLATE=C

  if [[ ! "$today" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]]; then
    echo "error: external publish pin clock must be a UTC YYYY-MM-DD date" >&2
    return 1
  fi
  while IFS='|' read -r crate precise_version expires_on reason; do
    [[ -n "$crate$precise_version$expires_on$reason" ]] || continue
    if [[ ! "$crate" =~ ^[A-Za-z0-9_-]+$ || \
          ! "$precise_version" =~ ^[0-9][A-Za-z0-9.+-]*$ || \
          ! "$expires_on" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ || \
          -z "$reason" ]]; then
      echo "error: malformed external publish pin for $crate" >&2
      return 1
    fi
    if [[ "$today" < "$expires_on" ]]; then
      printf 'external_publish_pin state=active crate=%s version=%s expires_on=%s reason=%s\n' \
        "$crate" "$precise_version" "$expires_on" "$reason"
      cargo update --manifest-path "$manifest" -p "$crate" --precise "$precise_version"
    else
      printf 'external_publish_pin state=expired crate=%s version=%s expires_on=%s reason=%s\n' \
        "$crate" "$precise_version" "$expires_on" "$reason"
      if [[ "${GITHUB_ACTIONS:-}" == "true" ]]; then
        echo "::warning title=External publish pin expired::$crate $precise_version expired on $expires_on; resolver-latest is checking current registry versions. $reason"
      fi
    fi
  done <<<"$external_publish_pins_table"
}

# Own both builds over one frozen set of extracted archives. Only Cargo.lock
# changes between them. A byte-identical minimum lockfile reuses the successful
# latest build; the minimum metadata read and its contract assertions still run.
package_verify_check_resolutions() {
  local manifest="$1" target="$2" build_dir="$3" scratch="$4" filter="$5"
  shift 5
  local latest_metadata="$scratch/resolver-latest-metadata.json"
  local minimum_metadata="$scratch/declared-minimum-metadata.json"
  local latest_lock="$scratch/resolver-latest.lock"
  local lockfile
  lockfile="$(dirname "$manifest")/Cargo.lock" || return $?
  local minimum_rows dependency minimum

  cargo generate-lockfile --manifest-path "$manifest" || return $?
  apply_external_publish_pins "$manifest" || return $?
  echo "=== Check all extracted publishable packages with resolver-latest dependencies ==="
  HARN_REQUIRE_CLI_AOT=1 CARGO_TARGET_DIR="$target" CARGO_BUILD_BUILD_DIR="$build_dir" \
    cargo check --locked --workspace --manifest-path "$manifest" || return $?
  cargo metadata --locked --format-version 1 --manifest-path "$manifest" \
    >"$latest_metadata" || return $?
  if [[ "$#" -eq 0 ]]; then
    return 0
  fi
  package_verify_emit_resolution_receipts resolver-latest "$latest_metadata" 0 "$filter" "$@" || return $?
  if [[ ! -s "$lockfile" ]]; then
    echo 'error: successful latest build has no nonempty lockfile to prove reuse' >&2
    return 1
  fi
  cp "$lockfile" "$latest_lock" || return $?
  minimum_rows="$(package_verify_select_minimums "$latest_metadata" "$filter" "$@")" || return $?
  while IFS=$'\t' read -r dependency minimum; do
    [[ -n "$dependency" ]] || continue
    cargo update --manifest-path "$manifest" -p "$dependency" --precise "$minimum" || return $?
  done <<<"$minimum_rows"

  if cmp -s "$latest_lock" "$lockfile"; then
    echo "=== Reuse resolver-latest compilation: declared-minimum lockfile is identical ==="
  else
    echo "=== Check all extracted publishable packages with declared-minimum dependencies ==="
    HARN_REQUIRE_CLI_AOT=1 CARGO_TARGET_DIR="$target" CARGO_BUILD_BUILD_DIR="$build_dir" \
      cargo check --locked --workspace --manifest-path "$manifest" || return $?
  fi
  cargo metadata --locked --format-version 1 --manifest-path "$manifest" \
    >"$minimum_metadata" || return $?
  package_verify_emit_resolution_receipts declared-minimum "$minimum_metadata" 1 "$filter" "$@"
}

package_verify_emit_resolution_receipts() {
  local phase="$1" metadata_path="$2" require_minimum="$3" filter="$4"
  shift 4
  local row package package_version dependency requirement minimum resolution_name resolved
  for row in "$@"; do
    IFS=$'\t' read -r package package_version dependency requirement minimum resolution_name <<<"$row"
    resolved="$(package_verify_resolved_version "$metadata_path" "$filter" \
      "$package" "$package_version" "$resolution_name")" || return $?
    # `none` describes a ceiling contract with no declared minimum to pin.
    if [[ "$require_minimum" == 1 && "$minimum" != none && "$resolved" != "$minimum" ]]; then
      echo "error: $phase resolved $package dependency $dependency to $resolved, expected minimum $minimum" >&2
      return 1
    fi
    printf 'dependency_resolution phase=%s package=%s@%s dependency=%s requirement=%s minimum=%s resolved=%s\n' \
      "$phase" "$package" "$package_version" "$dependency" "$requirement" "$minimum" "$resolved"
  done
}

package_verify_resolved_version() {
  local metadata_path=$1 filter=$2 package=$3 package_version=$4 resolution_name=$5
  jq -er --arg package "$package" --arg package_version "$package_version" \
    --arg resolution_name "$resolution_name" -f "$filter" "$metadata_path"
}

package_verify_select_minimums() {
  local metadata_path=$1 filter=$2
  shift 2
  local row _package _package_version dependency _requirement minimum _resolution_name
  local selected=()
  local entry selected_dependency selected_minimum found
  for row in "$@"; do
    IFS=$'\t' read -r _package _package_version dependency _requirement minimum _resolution_name <<<"$row"
    [[ "$minimum" != none ]] || continue
    found=0
    # Bash 3.2 rejects an empty array expansion under nounset.
    if [[ "${#selected[@]}" -gt 0 ]]; then
      for entry in "${selected[@]}"; do
        IFS=$'\t' read -r selected_dependency selected_minimum <<<"$entry"
        if [[ "$selected_dependency" == "$dependency" ]]; then
          found=1
          if [[ "$selected_minimum" != "$minimum" ]]; then
            echo "error: dependency contracts disagree on minimum for $dependency: $selected_minimum vs $minimum" >&2
            return 1
          fi
        fi
      done
    fi
    if [[ "$found" -eq 0 ]]; then
      selected+=("$dependency"$'\t'"$minimum")
    fi
  done
  [[ "${#selected[@]}" -gt 0 ]] || return 0
  local package package_version resolution_name resolved measured needs_pin
  for entry in "${selected[@]}"; do
    IFS=$'\t' read -r selected_dependency selected_minimum <<<"$entry"
    measured=0
    needs_pin=0
    for row in "$@"; do
      IFS=$'\t' read -r package package_version dependency _requirement minimum resolution_name <<<"$row"
      [[ "$dependency" == "$selected_dependency" && "$minimum" != none ]] || continue
      resolved="$(package_verify_resolved_version "$metadata_path" "$filter" \
        "$package" "$package_version" "$resolution_name")" || return $?
      measured=$((measured + 1))
      [[ "$resolved" == "$selected_minimum" ]] || needs_pin=1
    done
    if [[ "$measured" -eq 0 ]]; then
      echo "error: no measured resolution for minimum dependency $selected_dependency" >&2
      return 1
    fi
    if [[ "$needs_pin" -eq 1 ]]; then
      printf '%s\n' "$entry"
    fi
  done
}
