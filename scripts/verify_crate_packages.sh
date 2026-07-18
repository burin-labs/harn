#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

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

plan_rows="$("./scripts/harn_bin.sh" run "$ROOT_DIR/scripts/verify_crate_packages_plan.harn" -- --metadata "$metadata_file" --root "$ROOT_DIR")"
target_dir=""
publishable_crates=()
publishable_package_rows=()

while IFS=$'\t' read -r row_kind package_name package_version manifest_path crate_dir; do
  case "$row_kind" in
    "")
      ;;
    target_directory)
      target_dir="$package_name"
      ;;
    package)
      publishable_crates+=("$package_name")
      publishable_package_rows+=("$package_name"$'\t'"$package_version"$'\t'"$manifest_path"$'\t'"$crate_dir")
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

stdlib_version="$(package_version harn-stdlib)"
modules_version="$(package_version harn-modules)"
vm_version="$(package_version harn-vm)"

cargo_package() {
  CARGO_BUILD_BUILD_DIR="$package_check_build_dir" cargo package "$@"
}

inspect_packaged_includes() {
  local package_dir="$1"
  local crate="$2"
  # Extracted crates must stay outside the workspace so Cargo checks them as
  # publish artifacts; the inspector therefore needs explicit out-of-root read
  # access.
  "./scripts/harn_bin.sh" run --no-sandbox "$ROOT_DIR/scripts/verify_crate_package_includes.harn" -- \
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
  rm -rf "$tmp/$crate-$version"
  tar -xzf "$crate_archive" -C "$tmp"
  inspect_packaged_includes "$tmp/$crate-$version" "$crate"
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

tar -xzf "$stdlib_crate" -C "$tmp"
stdlib_pkg="$tmp/harn-stdlib-$stdlib_version"
inspect_packaged_includes "$stdlib_pkg" "harn-stdlib"

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

echo "=== Check extracted harn-stdlib package ==="
CARGO_TARGET_DIR="$package_check_target_dir" \
  CARGO_BUILD_BUILD_DIR="$package_check_build_dir" \
  cargo check --manifest-path "$stdlib_pkg/Cargo.toml"

echo "=== Package harn-modules ==="
cargo_package -p harn-modules --allow-dirty --no-verify "${local_harn_patches[@]}"

modules_crate="$target_dir/package/harn-modules-$modules_version.crate"
if [[ ! -f "$modules_crate" ]]; then
  echo "error: expected package archive missing: $modules_crate" >&2
  exit 1
fi

tar -xzf "$modules_crate" -C "$tmp"
modules_pkg="$tmp/harn-modules-$modules_version"
inspect_packaged_includes "$modules_pkg" "harn-modules"

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

echo "=== Check extracted harn-modules package ==="
CARGO_TARGET_DIR="$package_check_target_dir" \
  CARGO_BUILD_BUILD_DIR="$package_check_build_dir" \
  cargo check --manifest-path "$modules_pkg/Cargo.toml" "${local_harn_patches[@]}"

# `harn-hostlib` is a workspace path dep of `harn-cli`. Verifying it
# packages cleanly here (a) catches scaffold issues for the crate on its
# own and (b) mirrors the per-crate audit pattern used for harn-modules
# above. The check is independent of the harn-cli step below — packaging
# harn-hostlib does not preempt cargo's "must exist on crates.io" lookup
# for harn-cli's version requirement on harn-hostlib.
package_and_inspect_no_verify harn-hostlib

# `harn-vm` embeds runtime fixtures and schemas. Build the packaged crate
# instead of only creating the tarball so workspace-relative `include_str!`
# references fail here, before a broken crate reaches crates.io.
echo "=== Package harn-vm ==="
cargo_package -p harn-vm --allow-dirty --no-verify "${local_harn_patches[@]}"
extract_package harn-vm "$vm_version"
vm_pkg="$tmp/harn-vm-$vm_version"

echo "=== Check extracted harn-vm package ==="
CARGO_TARGET_DIR="$package_check_target_dir" \
  CARGO_BUILD_BUILD_DIR="$package_check_build_dir" \
  cargo check --manifest-path "$vm_pkg/Cargo.toml" "${local_harn_patches[@]}"

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
  cli_pkg="$tmp/harn-cli-$cli_version"

  # harn-cli must package with the target-independent AOT payload, but its
  # build script cannot rely on sibling workspace sources after extraction.
  # This is the direct proof: require the payload and compile the tarball from
  # outside the workspace with the same local dependency patch set.
  echo "=== Check extracted harn-cli package with required AOT payload ==="
  HARN_REQUIRE_CLI_AOT=1 \
    CARGO_TARGET_DIR="$package_check_target_dir" \
    CARGO_BUILD_BUILD_DIR="$package_check_build_dir" \
    cargo check --manifest-path "$cli_pkg/Cargo.toml" "${local_harn_patches[@]}"
fi

echo "Package verification complete"
