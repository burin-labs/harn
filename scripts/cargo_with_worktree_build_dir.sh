#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck source=scripts/lib/cargo_env.sh
source "$script_dir/lib/cargo_env.sh"

if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
  CARGO_TARGET_DIR="$(
    cargo metadata --format-version=1 --no-deps \
      | python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])'
  )"
  export CARGO_TARGET_DIR
fi

harn_export_cargo_build_dir_under_target "$CARGO_TARGET_DIR" || true

exec cargo "$@"
