#!/usr/bin/env bash
# Require an actual checker measurement before applying the stdlib type ratchet.
set -euo pipefail

harn_bin="$(./scripts/harn_bin.sh --no-build --print)"
exec node ./scripts/ci/stdlib-strict-types.mjs "$harn_bin"
