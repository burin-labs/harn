#!/usr/bin/env bash
# Warm the shared Linux workspace-tests Cargo graph on refs/heads/main.
#
# Exact-SHA merge-group proof reuse skips the compile lanes on main push, and
# rust-cache save-if only persists from refs/heads/main. This script is the
# post-merge writer that keeps the next merge_group restore from compiling
# cold. It matches the compile shape used by rust-check-inputs and the
# colocated workspace-tests leg without re-running the suite.
#
# Pair with cache-workspace-crates=true on the writer: Swatinem otherwise
# strips workspace artifacts before save and merge_group still rebuilds every
# harn-* crate after an exact key hit (#5003).
set -euo pipefail

cargo build --locked --bin harn
cargo nextest run --locked --workspace --profile ci --no-run \
  -E 'not (test(test_linux_process_sandbox_catches_ten_process_escapes) or test(workspace_env_integration) or test(local_backend_execs_inside_session_outputs) or test(local_backend_timeout_is_enforced_without_shell_timeout_binary) or test(sandboxed_npm_install_resolves_file_tarball_dependency_offline))'
# Match rust-check-inputs' exact GitHub-owned security archive compile shape.
cargo nextest run --locked --workspace --profile ci --no-run \
  -E '(package(harn-vm) and binary(harn_vm)) or (package(harn-hostlib) and (test(local_backend_execs_inside_session_outputs) or test(local_backend_timeout_is_enforced_without_shell_timeout_binary) or test(sandboxed_npm_install_resolves_file_tarball_dependency_offline)))'

# Workspace-crate cache canary touch for #5003 hosted wall-time sampling.
