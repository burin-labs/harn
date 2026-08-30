#!/usr/bin/env bash
set -euo pipefail

pr_url="${DEVELOPMENT_BUMP_PR_URL:-}"
if [[ -z "$pr_url" ]]; then
  echo "error: DEVELOPMENT_BUMP_PR_URL is required" >&2
  exit 1
fi

# The preparation step restamps the versioned grammar-fitness receipt. Prove
# the resolved grammars still satisfy that receipt before allowing the PR to
# merge. A red corpus leaves the already-open cutover visible and unarmed.
HARN_TEST_ONE_NAME='parser_agreement_corpus::resolved_grammars_pass_the_versioned_fitness_corpus' \
  HARN_TEST_ONE_PACKAGE=harn-hostlib \
  HARN_TEST_ONE_BINARY=harn_hostlib \
  make test-one

gh pr merge "$pr_url" --auto --squash
