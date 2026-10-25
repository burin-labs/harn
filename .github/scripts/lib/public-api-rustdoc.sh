#!/usr/bin/env bash

# Rustdoc JSON is compiler-specific. Only an exact source and compiler context
# can replace documenting the baseline with this job's toolchain.
public_api_rustdoc_identity() {
  local compiler
  compiler=$(rustc -vV) || return 1
  [[ -n "$compiler" ]] || return 1
  jq -n --arg source "$1" --arg compiler "$compiler" \
    --arg flags "${RUSTFLAGS:-}" --arg encoded_flags "${CARGO_ENCODED_RUSTFLAGS:-}" \
    --arg target "${CARGO_BUILD_TARGET:-}" \
    '{schema: "harn.public_rustdoc.v1", source: $source, compiler: $compiler,
      flags: $flags, encoded_flags: $encoded_flags, target: $target,
      rustdoc_flags: "-Z unstable-options --output-format json"}'
}

public_api_rustdoc_checksums() {
  local out=$1 package crate digest
  [[ -s "$out/crates.tsv" ]] || return 1
  awk -F '\t' 'NF != 2 || $1 !~ /^[a-zA-Z0-9_-]+$/ || $2 !~ /^[a-zA-Z0-9_]+$/ { exit 1 }' \
    "$out/crates.tsv" || return 1
  [[ $(LC_ALL=C sort -u "$out/crates.tsv" | wc -l) -eq $(wc -l < "$out/crates.tsv") ]] || return 1
  digest=$(sha256_file_hex "$out/crates.tsv") || return 1
  printf '%s  crates.tsv\n' "$digest"
  while IFS=$'\t' read -r package crate; do
    [[ -s "$out/$crate.json" ]] || return 1
    digest=$(sha256_file_hex "$out/$crate.json") || return 1
    printf '%s  %s.json\n' "$digest" "$crate"
  done < "$out/crates.tsv"
}

public_api_rustdoc_valid() {
  local out=$1 source=$2 identity checksums
  [[ -f "$out/identity.json" && -f "$out/checksums" ]] || return 1
  identity=$(public_api_rustdoc_identity "$source") || return 1
  jq -e --argjson expected "$identity" '. == $expected' "$out/identity.json" >/dev/null || return 1
  checksums=$(public_api_rustdoc_checksums "$out") || return 1
  [[ "$checksums" == "$(cat "$out/checksums")" ]]
}

# One invocation shares the dependency graph across all publishable libraries.
publishable_rustdoc() {
  local out=$1 metadata target_dir package crate
  local -a packages=()
  mkdir -p "$out"
  metadata=$(cargo metadata --format-version 1 --no-deps --locked)
  target_dir=$(jq -r .target_directory <<<"$metadata")
  jq -r '.packages[] | select(.publish != [])
    | . as $package | .targets[] | select(.kind | index("lib") or index("rlib"))
    | [$package.name, (.name | gsub("-"; "_"))] | @tsv' <<<"$metadata" >"$out/crates.tsv"
  [[ -s "$out/crates.tsv" ]] || { echo 'No publishable libraries were measured.' >&2; return 1; }
  while IFS=$'\t' read -r package crate; do
    packages+=(-p "$package")
  done <"$out/crates.tsv"
  rm -f "$target_dir"/doc/*.json
  RUSTC_BOOTSTRAP=1 RUSTDOCFLAGS="-Z unstable-options --output-format json" \
    cargo doc --locked --no-deps --lib "${packages[@]}" >&2
  while IFS=$'\t' read -r package crate; do
    cp "$target_dir/doc/$crate.json" "$out/$crate.json"
  done <"$out/crates.tsv"
  public_api_rustdoc_identity "$(git rev-parse HEAD)" > "$out/identity.json"
  public_api_rustdoc_checksums "$out" > "$out/checksums"
}
