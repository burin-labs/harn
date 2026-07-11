#!/usr/bin/env bash
# Strict-types ratchet for the Harn stdlib.
#
# Runs `harn check --strict-types` over crates/harn-stdlib/src/stdlib and fails
# when any HARN-OWN-004 finding ("unvalidated boundary value used directly")
# appears outside the frontier exclusion list below. The strict-types checker
# flags boundary-sourced values (json_parse / llm_call without a schema /
# host_call results) that are field-accessed without narrowing via
# schema_expect(), a schema_is() guard, or a shape type annotation.
#
# HARN-OWN-004 is emitted as a *warning*, so a plain `harn check` exits 0 on it;
# this gate is what turns the strict-types class into a hard CI failure. It keys
# on the HARN-OWN-004 code specifically and deliberately ignores every other
# diagnostic (pre-existing HARN-TYP-* type errors, lint warnings, etc.) that a
# whole-directory check surfaces but that are out of scope for this ratchet.
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

# Resolve the checker: prefer a pre-built binary in $HARN_BIN (CI warms one),
# else fall back to `cargo run` so the gate works from a bare checkout.
if [[ -n "${HARN_BIN:-}" ]]; then
  harn=("$HARN_BIN")
else
  harn=(cargo run --quiet --bin harn --)
fi

# The check exits non-zero on pre-existing HARN-TYP errors in the tree; those
# are not this gate's concern, so capture output and drive the verdict off the
# HARN-OWN-004 findings alone.
out="$("${harn[@]}" check --strict-types "$STDLIB_DIR" 2>&1 || true)"

# Each finding is a `warning[HARN-OWN-004]: ...` line followed by a
# `    --> <file>:<line>:<col>` locator. Pull the locators for our code.
mapfile -t locators < <(
  printf '%s\n' "$out" | awk -v code="$CODE" '
    index($0, "[" code "]") { armed = 1; next }
    armed && /-->/ { print $2; armed = 0 }
  '
)

violations=()
for loc in "${locators[@]}"; do
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

echo "strict-types ratchet passed: no $CODE findings in the gated stdlib."
