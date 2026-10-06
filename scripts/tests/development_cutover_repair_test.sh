#!/usr/bin/env bash
set -euo pipefail
node --test "$(dirname "${BASH_SOURCE[0]}")/development_cutover_repair_test.mjs"
