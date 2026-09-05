#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"
# shellcheck source=scripts/lib/package_verify_bootstrap.sh
source "$ROOT_DIR/scripts/lib/package_verify_bootstrap.sh"

VERIFY_CLI=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --verify-cli)
      VERIFY_CLI=1
      shift
      ;;
    *)
      echo "error: unknown arg: $1" >&2
      echo "usage: ./scripts/verify_crate_packages.sh [--verify-cli]" >&2
      exit 1
      ;;
  esac
done

metadata="$(cargo metadata --format-version 1 --no-deps)"
metadata_parent="$ROOT_DIR/.harn-package-verify-tmp"
mkdir -p "$metadata_parent"
metadata_tmp="$(mktemp -d "$metadata_parent/metadata.XXXXXX")"

scratch_parent="${HARN_PACKAGE_VERIFY_SCRATCH_DIR:-}"
if [[ -z "$scratch_parent" ]]; then
  tmp_root="$(cd "${TMPDIR:-/tmp}" && pwd)"
  case "$tmp_root" in
    "$ROOT_DIR"|"$ROOT_DIR"/*)
      scratch_parent="$(dirname "$ROOT_DIR")/.harn-package-verify-tmp"
      ;;
    *)
      scratch_parent="$tmp_root"
      ;;
  esac
else
  mkdir -p "$scratch_parent"
fi
mkdir -p "$scratch_parent"
tmp="$(mktemp -d "$scratch_parent/package-verify.XXXXXX")"
trap 'rm -rf "$tmp" "$metadata_tmp"' EXIT
metadata_file="$metadata_tmp/cargo-metadata.json"
printf '%s\n' "$metadata" >"$metadata_file"

# Build Harn and the AOT generator in one Cargo feature-unification boundary,
# generate/check the payload directly, and export the exact Harn executable.
# `$tmp` is trapped above, so it bounds the generator snapshot's lifetime.
package_verify_prepare_tools "$ROOT_DIR" "$tmp/aot-tools"
plan_rows="$("$HARN_BIN" run "$ROOT_DIR/scripts/verify_crate_packages_plan.harn" -- --metadata "$metadata_file" --root "$ROOT_DIR")"
target_dir=""
publishable_crates=()
publishable_package_rows=()
packaged_workspace_rows=()
dependency_contract_rows=()

while IFS=$'\t' read -r row_kind field1 field2 field3 field4 field5 field6; do
  case "$row_kind" in
    "")
      ;;
    target_directory)
      target_dir="$field1"
      ;;
    package)
      publishable_crates+=("$field1")
      publishable_package_rows+=("$field1"$'\t'"$field2"$'\t'"$field3"$'\t'"$field4")
      ;;
    dependency_contract)
      dependency_contract_rows+=("$field1"$'\t'"$field2"$'\t'"$field3"$'\t'"$field4"$'\t'"$field5"$'\t'"$field6")
      ;;
    *)
      echo "error: unknown verify_crate_packages_plan row kind: $row_kind" >&2
      exit 1
      ;;
  esac
done <<<"$plan_rows"

if [[ -z "$target_dir" ]]; then
  echo "error: verify_crate_packages_plan did not report target_directory" >&2
  exit 1
fi

package_check_target_dir="${HARN_PACKAGE_VERIFY_TARGET_DIR:-$target_dir/package-check-target}"
package_check_build_dir="${HARN_PACKAGE_VERIFY_BUILD_DIR:-$package_check_target_dir/build}"
mkdir -p "$package_check_target_dir" "$package_check_build_dir"
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"

# Verify the candidate workspace as a coherent publish set instead of
# resolving workspace-local dependencies to whatever versions are already
# on crates.io. That catches package-only failures without needing a
# prior bootstrap publish for newly split crates.
local_harn_patches=()
for row in "${publishable_package_rows[@]}"; do
  IFS=$'\t' read -r crate _version manifest_path _crate_dir <<<"$row"
  crate_dir="$(cd "$(dirname "$manifest_path")" && pwd)"
  local_harn_patches+=(--config "patch.crates-io.$crate.path=\"$crate_dir\"")
done

package_version() {
  local crate="$1"
  local row name version _manifest_path _crate_dir
  for row in "${publishable_package_rows[@]}"; do
    IFS=$'\t' read -r name version _manifest_path _crate_dir <<<"$row"
    if [[ "$name" == "$crate" ]]; then
      printf '%s\n' "$version"
      return 0
    fi
  done
  echo "error: package version not found for $crate" >&2
  return 1
}

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
    return
  fi
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
    return
  fi
  echo "error: sha256sum or shasum is required for packaged-crate receipts" >&2
  return 1
}

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
  local row package package_version dependency requirement minimum resolution_name resolved
  for row in "${dependency_contract_rows[@]}"; do
    IFS=$'\t' read -r package package_version dependency requirement minimum resolution_name <<<"$row"
    resolved="$(resolved_dependency_version \
      "$metadata_path" "$package" "$package_version" "$resolution_name")"
    if [[ "$require_minimum" == "1" && "$resolved" != "$minimum" ]]; then
      echo "error: $phase resolved $package dependency $dependency to $resolved, expected minimum $minimum" >&2
      return 1
    fi
    printf 'dependency_resolution phase=%s package=%s@%s dependency=%s requirement=%s resolved=%s\n' \
      "$phase" "$package" "$package_version" "$dependency" "$requirement" "$resolved"
  done
}

select_dependency_minimums() {
  local row _package _package_version dependency _requirement minimum _resolution_name
  local selected=()
  local entry selected_dependency selected_minimum found
  for row in "${dependency_contract_rows[@]}"; do
    IFS=$'\t' read -r _package _package_version dependency _requirement minimum _resolution_name <<<"$row"
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
  printf '%s\n' "${selected[@]}"
}

# `check_packaged_workspace`'s resolver-latest phase deliberately carries no
# lock: it resolves against whatever is newest on crates.io today, so it
# always builds the exact set a fresh downstream `cargo add` would get right
# now. That is also its whole exposure — any transitive dependency that
# ships a compile-broken release fails this workspace with no relation to a
# Harn code change, and `--locked`/`--frozen` cannot help (there is no lock
# for that synthetic workspace to honor). Each row below is one such known
# release excluded by pinning it to the last good version, so the next
# external break is one row to add and the fix is one row to delete once the
# crate publishes a working release. `crate` / `precise_version` feed
# `cargo update --manifest-path ... -p <crate> --precise <precise_version>`;
# `reason` and `date_added` are for humans grepping this table, not read by
# any script.
#
# crate    precise_version  reason                                                date_added
external_publish_pins_table='
tinyvec  1.12.0           tinyvec 1.13.0 (published 2026-09-02) does not compile: `vec!` is unreachable in tinyvec.rs, an upstream defect  2026-09-03
'

apply_external_publish_pins() {
  local manifest="$1"
  local line crate precise_version rest
  while IFS= read -r line; do
    [[ -n "${line// /}" ]] || continue
    read -r crate precise_version rest <<<"$line"
    [[ -n "$crate" ]] || continue
    echo "=== Pinning $crate to $precise_version in the resolver-latest workspace (external_publish_pins_table: $rest) ==="
    cargo update --manifest-path "$manifest" -p "$crate" --precise "$precise_version"
  done <<<"$external_publish_pins_table"
}

stdlib_version="$(package_version harn-stdlib)"
modules_version="$(package_version harn-modules)"
vm_version="$(package_version harn-vm)"

cargo_package() {
  # `--frozen` (= `--locked` + `--offline`): the local `--config
  # patch.crates-io.<crate>.path=...` overrides above redirect several deps
  # to local workspace paths, and that source change makes cargo's verify
  # build treat the graph as needing a fresh resolve even with `--locked`
  # alone, silently consulting the live registry and picking up a newly
  # published (and possibly broken) dependency version the workspace's own
  # `Cargo.lock` never saw. `--offline` removes the registry as an escape
  # hatch, so the build can only resolve from what the lock and local cache
  # already pin. Confirmed empirically: `--locked` alone still let a
  # verifying harn-cli build re-resolve tinyvec to a newer, broken release;
  # `--frozen` builds the workspace-pinned version.
  CARGO_BUILD_BUILD_DIR="$package_check_build_dir" cargo package --frozen "$@"
}

inspect_packaged_includes() {
  local package_dir="$1"
  local crate="$2"
  # Extracted crates must stay outside the workspace so Cargo checks them as
  # publish artifacts; the inspector therefore needs explicit out-of-root read
  # access.
  "$HARN_BIN" run --no-sandbox "$ROOT_DIR/scripts/verify_crate_package_includes.harn" -- \
    --package-dir "$package_dir" \
    --crate "$crate"
}

extract_package() {
  local crate="$1"
  local version="$2"
  local crate_archive="$target_dir/package/$crate-$version.crate"
  if [[ ! -f "$crate_archive" ]]; then
    echo "error: expected package archive missing: $crate_archive" >&2
    exit 1
  fi
  printf 'packaged_crate package=%s@%s archive=%s sha256=%s\n' \
    "$crate" "$version" "$crate_archive" "$(sha256_file "$crate_archive")"
  rm -rf "$tmp/$crate-$version"
  tar -xzf "$crate_archive" -C "$tmp"
  inspect_packaged_includes "$tmp/$crate-$version" "$crate"
  packaged_workspace_rows+=("$crate"$'\t'"$tmp/$crate-$version")
}

check_packaged_workspace() {
  local workspace_manifest="$tmp/Cargo.toml"
  local row crate package_dir package_rel
  local resolver_metadata="$tmp/resolver-latest-metadata.json"
  local minimum_metadata="$tmp/declared-minimum-metadata.json"
  local dependency minimum minimum_rows

  # Cargo's normalized package manifests have workspace inheritance and path
  # dependencies removed. Reassemble those exact registry candidates into one
  # temporary workspace, then patch their registry dependencies to the other
  # extracted candidates. One resolver/build graph checks every publishable
  # archive without recompiling overlapping dependency graphs per root.
  {
    printf '[workspace]\nresolver = "2"\nmembers = [\n'
    for row in "${packaged_workspace_rows[@]}"; do
      IFS=$'\t' read -r crate package_dir <<<"$row"
      package_rel="${package_dir#"$tmp"/}"
      printf '  "%s",\n' "$package_rel"
    done
    printf ']\n\n[patch.crates-io]\n'
    for row in "${packaged_workspace_rows[@]}"; do
      IFS=$'\t' read -r crate package_dir <<<"$row"
      package_rel="${package_dir#"$tmp"/}"
      printf '"%s" = { path = "%s" }\n' "$crate" "$package_rel"
    done
  } >"$workspace_manifest"

  cargo generate-lockfile --manifest-path "$workspace_manifest"
  apply_external_publish_pins "$workspace_manifest"

  echo "=== Check all extracted publishable packages with resolver-latest dependencies ==="
  HARN_REQUIRE_CLI_AOT=1 \
    CARGO_TARGET_DIR="$package_check_target_dir" \
    CARGO_BUILD_BUILD_DIR="$package_check_build_dir" \
    cargo check --workspace --manifest-path "$workspace_manifest"

  cargo metadata --locked --format-version 1 --manifest-path "$workspace_manifest" \
    >"$resolver_metadata"
  emit_dependency_resolution_receipts resolver-latest "$resolver_metadata" 0

  if [[ "${#dependency_contract_rows[@]}" -eq 0 ]]; then
    return
  fi

  minimum_rows="$(select_dependency_minimums)"
  while IFS=$'\t' read -r dependency minimum; do
    [[ -n "$dependency" ]] || continue
    cargo update --manifest-path "$workspace_manifest" -p "$dependency" --precise "$minimum"
  done <<<"$minimum_rows"

  echo "=== Check all extracted publishable packages with declared-minimum dependencies ==="
  HARN_REQUIRE_CLI_AOT=1 \
    CARGO_TARGET_DIR="$package_check_target_dir" \
    CARGO_BUILD_BUILD_DIR="$package_check_build_dir" \
    cargo check --locked --workspace --manifest-path "$workspace_manifest"
  cargo metadata --locked --format-version 1 --manifest-path "$workspace_manifest" \
    >"$minimum_metadata"
  emit_dependency_resolution_receipts declared-minimum "$minimum_metadata" 1
}

package_and_inspect_no_verify() {
  local crate="$1"
  local version
  version="$(package_version "$crate")"
  echo "=== Package $crate ==="
  cargo_package -p "$crate" --allow-dirty --no-verify "${local_harn_patches[@]}"
  extract_package "$crate" "$version"
}

echo "=== Package and inspect baseline crates ==="
while IFS= read -r crate; do
  case "$crate" in
    harn-stdlib|harn-modules|harn-hostlib|harn-vm|harn-cli)
      ;;
    *)
      package_and_inspect_no_verify "$crate"
      ;;
  esac
done < <(
  printf '%s\n' "${publishable_crates[@]}"
)

echo "=== Package harn-stdlib ==="
cargo_package -p harn-stdlib --allow-dirty --no-verify "${local_harn_patches[@]}"

stdlib_crate="$target_dir/package/harn-stdlib-$stdlib_version.crate"
if [[ ! -f "$stdlib_crate" ]]; then
  echo "error: expected package archive missing: $stdlib_crate" >&2
  exit 1
fi

extract_package harn-stdlib "$stdlib_version"
stdlib_pkg="$tmp/harn-stdlib-$stdlib_version"

echo "=== Inspect harn-stdlib package contents ==="
while IFS= read -r source; do
  rel="${source#crates/harn-stdlib/}"
  packaged="$stdlib_pkg/$rel"
  if [[ ! -f "$packaged" ]]; then
    echo "error: packaged harn-stdlib is missing $rel" >&2
    exit 1
  fi
  if ! cmp -s "$source" "$packaged"; then
    echo "error: packaged $rel differs from $source" >&2
    exit 1
  fi
done < <(find crates/harn-stdlib/src/stdlib -maxdepth 1 -name 'stdlib*.harn' -print | sort)

# ERE (-E) so the alternation is portable: BRE `\|` is a GNU-grep extension and
# matches literally on BSD/macOS grep, which would make this guard a silent no-op
# on the primary dev platform.
if grep -RE '\.\./harn-(vm|modules)' "$stdlib_pkg/src" >/dev/null; then
  echo "error: packaged harn-stdlib contains workspace-relative consumer includes" >&2
  exit 1
fi

echo "=== Package harn-modules ==="
cargo_package -p harn-modules --allow-dirty --no-verify "${local_harn_patches[@]}"

modules_crate="$target_dir/package/harn-modules-$modules_version.crate"
if [[ ! -f "$modules_crate" ]]; then
  echo "error: expected package archive missing: $modules_crate" >&2
  exit 1
fi

extract_package harn-modules "$modules_version"
modules_pkg="$tmp/harn-modules-$modules_version"

echo "=== Inspect harn-modules package stdlib use ==="
if [[ -e "$modules_pkg/src/stdlib" ]]; then
  echo "error: packaged harn-modules still contains a mirrored src/stdlib tree" >&2
  exit 1
fi

# ERE (-E): see the harn-stdlib guard above — BRE `\|` no-ops on BSD/macOS grep.
if grep -RE '\.\./harn-(vm|stdlib)' "$modules_pkg/src" >/dev/null; then
  echo "error: packaged harn-modules contains workspace-relative stdlib includes" >&2
  exit 1
fi

# `harn-hostlib` is a workspace path dep of `harn-cli`. Verifying it
# packages cleanly here (a) catches scaffold issues for the crate on its
# own and (b) mirrors the per-crate audit pattern used for harn-modules
# above. The check is independent of the harn-cli step below — packaging
# harn-hostlib does not preempt cargo's "must exist on crates.io" lookup
# for harn-cli's version requirement on harn-hostlib.
package_and_inspect_no_verify harn-hostlib

# `harn-vm` embeds runtime fixtures and schemas. Its extracted archive joins
# the exact packaged workspace below so workspace-relative `include_str!`
# references fail here, before a broken crate reaches crates.io.
echo "=== Package harn-vm ==="
cargo_package -p harn-vm --allow-dirty --no-verify "${local_harn_patches[@]}"
extract_package harn-vm "$vm_version"

echo "=== Package harn-cli ==="
if [[ "${HARN_BOOTSTRAP_NEW_CRATES:-0}" == "1" ]]; then
  echo "=== HARN_BOOTSTRAP_NEW_CRATES=1: skipping harn-cli package check ==="
elif [[ "$VERIFY_CLI" -eq 1 ]]; then
  cargo_package -p harn-cli --allow-dirty "${local_harn_patches[@]}"
else
  cargo_package -p harn-cli --allow-dirty --no-verify "${local_harn_patches[@]}"
fi
if [[ "${HARN_BOOTSTRAP_NEW_CRATES:-0}" != "1" ]]; then
  cli_version="$(package_version harn-cli)"
  extract_package harn-cli "$cli_version"
fi

# harn-cli must package with the target-independent AOT payload, and every
# publishable crate must compile from its normalized archive rather than its
# workspace source manifest. This is the direct combined proof.
check_packaged_workspace

echo "Package verification complete"
