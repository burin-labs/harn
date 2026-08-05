#!/usr/bin/env bash
# Zero-baseline strict gate for the std/schema module facade.
#
# The zod-like std/schema boundary publishes the canonical typed contracts for
# schema issues, failures, reports, results, and validator objects (see
# crates/harn-stdlib/src/stdlib/schema/contracts.harn). Because every other
# stdlib consumer imports those contracts, the facade itself must stay
# strict-clean with no grandfathered findings: it is the one source of truth for
# the runtime shapes.
#
# Unlike scripts/check_stdlib_strict_types.sh (a directory-wide HARN-OWN-004
# ratchet with a shrinking frontier) and scripts/check_stdlib_public_return_types.sh
# (a baseline of remaining HARN-STD-102 debt), this gate is zero-tolerance for
# the two schema-owning files: both `harn check --strict-types` and
# `harn lint --strict` must exit 0. Never add an exclusion list here — fix the
# finding at the source instead.
set -euo pipefail

FILES=(
  "crates/harn-stdlib/src/stdlib/stdlib_schema.harn"
  "crates/harn-stdlib/src/stdlib/schema/contracts.harn"
)

# Resolve the checker up front so a resolver failure fails the gate rather than
# being mistaken for a clean run.
harn_bin="$(./scripts/harn_bin.sh --no-build --print)"

status=0
for file in "${FILES[@]}"; do
  if ! "$harn_bin" check --strict-types "$file"; then
    echo "strict-types gate failed for $file" >&2
    status=1
  fi
  if ! "$harn_bin" lint --strict "$file"; then
    echo "strict lint gate failed for $file" >&2
    status=1
  fi
done

if [[ "$status" -ne 0 ]]; then
  echo >&2
  echo "The std/schema facade must stay strict-clean. Add honest types at the" >&2
  echo "source (contracts.harn owns the runtime shapes); do not grandfather" >&2
  echo "findings or add exclusions to this gate." >&2
  exit 1
fi

echo "schema strict gate passed: std/schema facade is strict-types and strict-lint clean."
