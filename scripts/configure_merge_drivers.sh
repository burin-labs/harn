#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

git config merge.harn-generated.name "Keep current generated file during merge; regenerate after merge"
git config merge.harn-generated.driver true

echo "Configured Harn merge drivers"
