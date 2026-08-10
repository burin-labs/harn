#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
workflow="${repo_root}/.github/workflows/e2e.yml"

require_line() {
  local expected="$1"
  if ! grep -Fqx -- "${expected}" "${workflow}"; then
    printf 'missing E2E workflow contract: %s\n' "${expected}" >&2
    exit 1
  fi
}

# Adding the label opts a PR into the slow tier; every later commit must be
# checked too, while superseded work is cancelled only by that new commit.
require_line '    types: [labeled, synchronize]'
require_line "  group: \${{ github.workflow }}-\${{ github.event.pull_request.number || github.ref }}"
require_line "  cancel-in-progress: \${{ github.event_name == 'pull_request' && github.event.action == 'synchronize' }}"

# The synchronize trigger remains cost-gated by the PR's durable label state.
if ! grep -Fq "contains(github.event.pull_request.labels.*.name, 'e2e')" "${workflow}"; then
  echo 'E2E pull-request runs are no longer gated by the e2e label' >&2
  exit 1
fi

echo 'E2E workflow trigger contract passed'
