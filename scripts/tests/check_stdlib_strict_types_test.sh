#!/usr/bin/env bash
set -euo pipefail
exec node "$(dirname "${BASH_SOURCE[0]}")/stdlib_strict_types_test.mjs"
