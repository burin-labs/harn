#!/usr/bin/env bash

# Read every declared dependency through its package-specific Cargo edge.
# The package plan owns the contract rows; this boundary owns their selection
# and measured resolver receipts.

resolved_dependency_version() {
  local metadata_path="$1"
  local package="$2"
  local package_version="$3"
  local resolution_name="$4"
  jq -er \
    --arg package "$package" \
    --arg package_version "$package_version" \
    --arg resolution_name "$resolution_name" \
    -f "$ROOT_DIR/scripts/verify_crate_dependency_resolution.jq" \
    "$metadata_path"
}

emit_dependency_resolution_receipts() {
  local phase="$1"
  local metadata_path="$2"
  local require_minimum="$3"
  shift 3
  local row package package_version dependency requirement minimum resolution_name resolved
  for row in "$@"; do
    IFS=$'\t' read -r package package_version dependency requirement minimum resolution_name <<<"$row"
    resolved="$(resolved_dependency_version \
      "$metadata_path" "$package" "$package_version" "$resolution_name")" || return $?
    # A ceiling-only contract has no floor to assert, but still needs a
    # measured receipt in both phases. `none` preserves the TSV column.
    if [[ "$require_minimum" == "1" && "$minimum" != "none" && "$resolved" != "$minimum" ]]; then
      echo "error: $phase resolved $package dependency $dependency to $resolved, expected minimum $minimum" >&2
      return 1
    fi
    printf 'dependency_resolution phase=%s package=%s@%s dependency=%s requirement=%s minimum=%s resolved=%s\n' \
      "$phase" "$package" "$package_version" "$dependency" "$requirement" "$minimum" "$resolved"
  done
}

select_dependency_minimums() {
  local metadata_path="$1"
  shift
  local row package package_version dependency requirement minimum resolution_name
  local selected=()
  local entry selected_dependency selected_minimum found resolved needs_update
  for row in "$@"; do
    IFS=$'\t' read -r package package_version dependency requirement minimum resolution_name <<<"$row"
    if [[ "$minimum" == "none" ]]; then
      continue
    fi
    found=0
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
    if [[ "$found" -eq 0 ]]; then
      selected+=("$dependency"$'\t'"$minimum")
    fi
  done
  if [[ "${#selected[@]}" -eq 0 ]]; then
    return 0
  fi
  for entry in "${selected[@]}"; do
    IFS=$'\t' read -r selected_dependency selected_minimum <<<"$entry"
    needs_update=0
    for row in "$@"; do
      IFS=$'\t' read -r package package_version dependency requirement minimum resolution_name <<<"$row"
      if [[ "$dependency" != "$selected_dependency" || "$minimum" == "none" ]]; then
        continue
      fi
      resolved="$(resolved_dependency_version \
        "$metadata_path" "$package" "$package_version" "$resolution_name")" || return $?
      if [[ "$resolved" != "$selected_minimum" ]]; then
        needs_update=1
      fi
    done
    if [[ "$needs_update" -eq 1 ]]; then
      printf '%s\n' "$entry"
    fi
  done
}
