#!/usr/bin/env bash

# Shared bootstrap for package verification. One Cargo invocation owns both
# current-tree executables so Cargo can unify features across harn-cli and the
# narrower AOT generator instead of compiling their shared graph twice.

package_verify_prepare_tools() {
  local root_dir="$1"
  local aot_generator
  local generated_dir="$root_dir/crates/harn-cli/generated"
  local resolved
  local target_dir

  if [[ -n "${HARN_BIN:-}" ]]; then
    # Release orchestration supplies a stable exact-tree Harn copy so parallel
    # Rust builds cannot relink the executable while package scripts use it.
    # Preserve that boundary and build only the generator it does not supply.
    (cd "$root_dir" && "$root_dir/scripts/cargo_with_worktree_build_dir.sh" build \
      -p harn-cli-aot-gen --bin harn-cli-aot-gen)
  else
    # Cold CI owns neither tool. Select both in one Cargo invocation so their
    # shared dependencies compile with one feature-unified graph.
    (cd "$root_dir" && "$root_dir/scripts/cargo_with_worktree_build_dir.sh" build \
      -p harn-cli --bin harn \
      -p harn-cli-aot-gen --bin harn-cli-aot-gen)
  fi
  resolved="$(cd "$root_dir" && HARN_BIN_NO_BUILD=1 \
    "$root_dir/scripts/harn_bin.sh" --print)"
  if [[ ! -x "$resolved" ]]; then
    echo "error: package verification Harn binary is not executable: $resolved" >&2
    return 1
  fi
  # shellcheck source=scripts/lib/cargo_env.sh
  source "$root_dir/scripts/lib/cargo_env.sh"
  target_dir="${CARGO_TARGET_DIR:-}"
  if [[ -z "$target_dir" ]]; then
    target_dir="$(cd "$root_dir" && harn_cargo_metadata_target_dir)"
  fi
  aot_generator="$target_dir/debug/harn-cli-aot-gen"
  if [[ ! -x "$aot_generator" && -x "$aot_generator.exe" ]]; then
    aot_generator+=".exe"
  fi
  if [[ ! -x "$aot_generator" ]]; then
    echo "error: package verification AOT generator is not executable: $aot_generator" >&2
    return 1
  fi
  HARN_BIN="$resolved"
  export HARN_BIN
  # release_gate.sh generates once before its parallel readers start. Repair a
  # genuinely absent/partial payload here, but never rewrite a complete one.
  if [[ ! -f "$generated_dir/cli-bytecode-manifest.json" || \
        ! -d "$generated_dir/cli-bytecode" ]]; then
    "$aot_generator" --workspace-root "$root_dir"
  fi
  "$aot_generator" --workspace-root "$root_dir" --check
}
