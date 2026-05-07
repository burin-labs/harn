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
target_dir="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])' <<<"$metadata")"
stdlib_version="$(
  python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "harn-stdlib"))' \
    <<<"$metadata"
)"
modules_version="$(
  python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "harn-modules"))' \
    <<<"$metadata"
)"
vm_version="$(
  python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "harn-vm"))' \
    <<<"$metadata"
)"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
metadata_file="$tmp/cargo-metadata.json"
printf '%s\n' "$metadata" >"$metadata_file"

# Verify the candidate workspace as a coherent publish set instead of
# resolving intra-Harn dependencies to whatever versions are already on
# crates.io. That catches package-only failures without needing a prior
# bootstrap publish for newly split crates.
local_harn_patches=()
while IFS=$'\t' read -r crate manifest_path; do
  crate_dir="$(cd "$(dirname "$manifest_path")" && pwd)"
  local_harn_patches+=(--config "patch.crates-io.$crate.path=\"$crate_dir\"")
done < <(
  python3 - "$metadata_file" <<'PY'
import json
import pathlib
import sys

metadata = json.loads(pathlib.Path(sys.argv[1]).read_text())
members = set(metadata["workspace_members"])
for package in sorted(metadata["packages"], key=lambda p: p["name"]):
    if (
        package["id"] in members
        and package["name"].startswith("harn-")
        and package.get("publish") != []
    ):
        print(f'{package["name"]}\t{package["manifest_path"]}')
PY
)

package_version() {
  local crate="$1"
  python3 - "$crate" "$metadata_file" <<'PY'
import json
import sys
import pathlib

crate = sys.argv[1]
metadata = json.loads(pathlib.Path(sys.argv[2]).read_text())
print(next(p["version"] for p in metadata["packages"] if p["name"] == crate))
PY
}

inspect_packaged_includes() {
  local package_dir="$1"
  local crate="$2"
  python3 - "$package_dir" "$crate" <<'PY'
import ast
import pathlib
import re
import sys

root = pathlib.Path(sys.argv[1]).resolve()
crate = sys.argv[2]
errors = []
normal = re.compile(r'include_(?:str|bytes)!\s*\(\s*"((?:\\.|[^"\\])*)"\s*\)', re.S)
raw = re.compile(r'include_(?:str|bytes)!\s*\(\s*r(?P<hashes>#*)"(?P<path>.*?)"(?P=hashes)\s*\)', re.S)

for source in sorted(root.rglob("*.rs")):
    text = source.read_text()
    matches = []
    for match in normal.finditer(text):
        try:
            relative = ast.literal_eval('"' + match.group(1) + '"')
        except Exception as exc:
            errors.append(f"{source.relative_to(root)}: cannot decode include path: {exc}")
            continue
        matches.append(relative)
    matches.extend(match.group("path") for match in raw.finditer(text))

    for relative in matches:
        if pathlib.PurePosixPath(relative).is_absolute():
            errors.append(f"{source.relative_to(root)}: absolute include path {relative!r}")
            continue
        target = (source.parent / relative).resolve()
        if not target.exists():
            errors.append(f"{source.relative_to(root)}: missing include target {relative!r}")
            continue
        try:
            target.relative_to(root)
        except ValueError:
            errors.append(f"{source.relative_to(root)}: include target escapes package: {relative!r}")

if errors:
    print(f"error: packaged {crate} has invalid include_str!/include_bytes! paths:", file=sys.stderr)
    for error in errors:
        print(f"  - {error}", file=sys.stderr)
    raise SystemExit(1)
PY
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
  cargo package -p "$crate" --allow-dirty --no-verify "${local_harn_patches[@]}"
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
  python3 - "$metadata_file" <<'PY'
import json
import sys
import pathlib

metadata = json.loads(pathlib.Path(sys.argv[1]).read_text())
members = set(metadata["workspace_members"])
for package in sorted(metadata["packages"], key=lambda p: p["name"]):
    if package["id"] in members and package.get("publish") != []:
        print(package["name"])
PY
)

echo "=== Package harn-stdlib ==="
cargo package -p harn-stdlib --allow-dirty "${local_harn_patches[@]}"

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

if grep -R '\.\./harn-\(vm\|modules\)' "$stdlib_pkg/src" >/dev/null; then
  echo "error: packaged harn-stdlib contains workspace-relative consumer includes" >&2
  exit 1
fi

echo "=== Check extracted harn-stdlib package ==="
CARGO_TARGET_DIR="$tmp/target-stdlib" cargo check --manifest-path "$stdlib_pkg/Cargo.toml"

echo "=== Package harn-modules ==="
cargo package -p harn-modules --allow-dirty --no-verify "${local_harn_patches[@]}"

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

if grep -R '\.\./harn-\(vm\|stdlib\)' "$modules_pkg/src" >/dev/null; then
  echo "error: packaged harn-modules contains workspace-relative stdlib includes" >&2
  exit 1
fi

echo "=== Check extracted harn-modules package ==="
CARGO_TARGET_DIR="$tmp/target-modules" \
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
cargo package -p harn-vm --allow-dirty --no-verify "${local_harn_patches[@]}"
extract_package harn-vm "$vm_version"
vm_pkg="$tmp/harn-vm-$vm_version"

echo "=== Check extracted harn-vm package ==="
CARGO_TARGET_DIR="$tmp/target-vm" \
  cargo check --manifest-path "$vm_pkg/Cargo.toml" "${local_harn_patches[@]}"

echo "=== Package harn-cli ==="
if [[ "$VERIFY_CLI" -eq 1 ]]; then
  cargo package -p harn-cli --allow-dirty "${local_harn_patches[@]}"
else
  cargo package -p harn-cli --allow-dirty --no-verify "${local_harn_patches[@]}"
fi
extract_package harn-cli "$(package_version harn-cli)"

echo "Package verification complete"
