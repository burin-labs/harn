#!/usr/bin/env bash
# Type-safety gate for the Harn stdlib.
#
# Runs `harn check --strict-types` over crates/harn-stdlib/src/stdlib and fails
# when any HARN-OWN-004 finding ("unvalidated boundary value used directly")
# appears outside the frontier exclusion list below. It also fails on every
# ordinary HARN-TYP-* error. Type errors have no baseline: shipped stdlib
# modules must remain statically usable even when an optional nightly command
# is the first consumer to reach one of them.
#
# The strict-types checker flags boundary-sourced values (json_parse / llm_call without a schema /
# host_call results) that are field-accessed without narrowing via
# schema_expect(), a schema_is() guard, or a shape type annotation.
#
# HARN-OWN-004 is emitted as a *warning*, so this gate turns the strict-types
# class into a hard CI failure. Directory-wide lint debt remains separately
# ratcheted; neither warnings nor its non-zero exit status can hide a type error.
#
# Frontier: files listed in EXCLUDE still carry HARN-OWN-004 findings whose
# clean fix is a judgment call, not an unambiguous narrowing (see PR that added
# this gate). They are waived here so the rest of the stdlib is ratcheted to
# zero. Shrink this list as the frontier files are fixed; never grow it to
# silence a new violation.
set -euo pipefail

STDLIB_DIR="crates/harn-stdlib/src/stdlib"
CODE="HARN-OWN-004"

# Repo-relative paths waived from the ratchet.
EXCLUDE=(
  "crates/harn-stdlib/src/stdlib/agent/user.harn"
  "crates/harn-stdlib/src/stdlib/stdlib_agents.harn"
)

# Resolve the checker before suppressing diagnostic exit status below. This
# gate must fail rather than compile or mistake resolver failure for no findings.
harn_bin="$(./scripts/harn_bin.sh --no-build --print)"

# The directory still has separately ratcheted lint warnings, so capture output
# and drive this gate from the diagnostic codes it owns.
out="$("$harn_bin" check --strict-types "$STDLIB_DIR" 2>&1 || true)"

type_errors=()
while IFS= read -r type_error; do
  type_errors+=("$type_error")
done < <(
  printf '%s\n' "$out" | awk '
    index($0, "error[HARN-TYP-") { diagnostic = $0; armed = 1; next }
    armed && /-->/ { print diagnostic " @ " $2; armed = 0 }
  '
)

if [[ ${#type_errors[@]} -gt 0 ]]; then
  echo "stdlib type-safety gate failed: ordinary type errors are not baseline-eligible:" >&2
  for diagnostic in "${type_errors[@]}"; do
    echo "  $diagnostic" >&2
  done
  exit 1
fi

# Each finding is a `warning[HARN-OWN-004]: ...` line followed by a
# `    --> <file>:<line>:<col>` locator. Pull the locators for our code.
locators=()
while IFS= read -r locator; do
  locators+=("$locator")
done < <(
  printf '%s\n' "$out" | awk -v code="$CODE" '
    index($0, "[" code "]") { armed = 1; next }
    armed && /-->/ { print $2; armed = 0 }
  '
)

violations=()
for loc in "${locators[@]+"${locators[@]}"}"; do
  file="${loc%%:*}"
  skip=""
  for ex in "${EXCLUDE[@]}"; do
    if [[ "$file" == "$ex" ]]; then
      skip=1
      break
    fi
  done
  [[ -z "$skip" ]] && violations+=("$loc")
done

if [[ ${#violations[@]} -gt 0 ]]; then
  echo "strict-types ratchet failed: unvalidated boundary access ($CODE) in the gated stdlib:" >&2
  for v in "${violations[@]}"; do
    echo "  $v" >&2
  done
  echo >&2
  echo "Narrow the boundary value before field access: assign it to a variable and" >&2
  echo "validate with schema_expect(), guard with schema_is() in an if-condition, or" >&2
  echo "add a shape type annotation (e.g. \`const x: {field: T} = llm_call(...)\`)." >&2
  exit 1
fi

echo "stdlib type-safety gate passed: no HARN-TYP-* errors or unratcheted $CODE findings."
