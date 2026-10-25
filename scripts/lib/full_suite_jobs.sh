# shellcheck shell=bash
# The main-push CI jobs whose success makes a run full-suite proof. Promotion
# requires every one at the release source; retirement reads the same list to
# decide that a source can never satisfy that requirement.
# shellcheck disable=SC2034
RELEASE_FULL_SUITE_JOBS=(
  'CI status'
  'Verify publishable crates'
  'Stack frame budget'
  'Rust workspace tests'
  'Run Linux sandbox tests'
  'Windows cross-compile check'
  'Rust on macOS (deny-warnings build + lint)'
)
