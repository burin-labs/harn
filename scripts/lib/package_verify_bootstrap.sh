#!/usr/bin/env bash

# Shared bootstrap for package verification. The package audit needs the
# current-tree Harn executable for typed plan/inspection scripts and the CLI
# AOT payload for the extracted harn-cli build. Resolve the broader executable
# first so its harn-vm/harn-stdlib graph warms the narrower AOT generator.

package_verify_resolve_harn_bin() {
  local root_dir="$1"
  local resolved
  resolved="$("$root_dir/scripts/harn_bin.sh" --print)"
  if [[ ! -x "$resolved" ]]; then
    echo "error: package verification Harn binary is not executable: $resolved" >&2
    return 1
  fi
  HARN_BIN="$resolved"
  export HARN_BIN
}

package_verify_ensure_cli_aot() {
  local root_dir="$1"
  local manifest="$root_dir/crates/harn-cli/generated/cli-bytecode-manifest.json"
  if [[ ! -f "$manifest" ]]; then
    make --no-print-directory -C "$root_dir" gen-cli-aot
  fi
  make --no-print-directory -C "$root_dir" check-cli-aot
}
