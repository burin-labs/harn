#!/usr/bin/env bash

# Shared bootstrap for package verification. One Cargo invocation owns both
# current-tree executables so Cargo can unify features across harn-cli and the
# narrower AOT generator instead of compiling their shared graph twice.

package_verify_prepare_tools() {
  local root_dir="$1"
  local aot_generator
  local binary_name
  local resolved

  "$root_dir/scripts/cargo_with_worktree_build_dir.sh" build \
    -p harn-cli --bin harn \
    -p harn-cli-aot-gen --bin harn-cli-aot-gen
  resolved="$(HARN_BIN='' HARN_BIN_NO_BUILD=1 \
    "$root_dir/scripts/harn_bin.sh" --print)"
  if [[ ! -x "$resolved" ]]; then
    echo "error: package verification Harn binary is not executable: $resolved" >&2
    return 1
  fi
  binary_name="$(basename "$resolved")"
  case "$binary_name" in
    harn) aot_generator="$(dirname "$resolved")/harn-cli-aot-gen" ;;
    harn.exe) aot_generator="$(dirname "$resolved")/harn-cli-aot-gen.exe" ;;
    *)
      echo "error: package verification resolved unexpected Harn binary: $resolved" >&2
      return 1
      ;;
  esac
  if [[ ! -x "$aot_generator" ]]; then
    echo "error: package verification AOT generator is not executable: $aot_generator" >&2
    return 1
  fi
  HARN_BIN="$resolved"
  export HARN_BIN
  "$aot_generator" --workspace-root "$root_dir"
  "$aot_generator" --workspace-root "$root_dir" --check
}
