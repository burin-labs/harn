#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
workflow="${repo_root}/.github/workflows/e2e.yml"
reason_script="${repo_root}/scripts/ci/e2e_pull_request_reason.sh"

require_line() {
  local expected="$1"
  if ! grep -Fqx -- "${expected}" "${workflow}"; then
    printf 'missing CLI end-to-end workflow contract: %s\n' "${expected}" >&2
    exit 1
  fi
}

require_text() {
  local expected="$1"
  local why="$2"
  if ! grep -Fq -- "${expected}" "${workflow}"; then
    printf 'CLI end-to-end workflow contract broken (%s): %s\n' "${why}" "${expected}" >&2
    exit 1
  fi
}

# A pull request that touches a watched path must run the suite on its first
# head. With only `labeled` and `synchronize` it would wait for a later push
# that may never come.
require_line '    types: [opened, reopened, labeled, synchronize]'
require_line "  group: \${{ github.workflow }}-\${{ github.event.pull_request.number || github.ref }}"
require_line "  cancel-in-progress: \${{ github.event_name == 'pull_request' && github.event.action == 'synchronize' }}"

# The label remains the opt-in for every change outside the watched paths.
require_text "contains(github.event.pull_request.labels.*.name, 'e2e')" \
  'the e2e label is no longer read at all'

# The two surfaces that are their own reason to run, plus the workflow that
# decides it. A filter that stops naming one of these returns this repository
# to the defect that a change to the ACP session/new contract edited a test in
# this suite, ran nothing, and left main red for thirteen hours.
for watched in \
  "- '.github/workflows/e2e.yml'" \
  "- 'crates/harn-cli/tests/harn_cli_e2e/**'" \
  "- 'crates/harn-serve/src/adapters/acp/**'"; do
  require_text "${watched}" 'a watched path left the E2E filter'
done

# Read as "not false", never "is true". A detector that failed or was skipped
# leaves the output empty, and treating an unread decision as "do not run" is
# the silence this workflow exists to prevent.
require_text "needs.changes.outputs.should_run != 'false'" \
  'the suite gate no longer tolerates an unread decision'
if grep -Fq "needs.changes.outputs.should_run == 'true'" "${workflow}"; then
  echo 'the E2E gate reads should_run == true, so a failed detector silently skips the suite' >&2
  exit 1
fi

# A fork pull request cannot read the release-app credentials the availability
# probe needs, and must not be broken by them.
require_text 'github.event.pull_request.head.repo.full_name == github.repository' \
  'the availability probe is no longer fork-safe'

# The decision has to be reported, not only emitted.
if [[ ! -x "${reason_script}" ]]; then
  echo "missing executable detector reporter: ${reason_script}" >&2
  exit 1
fi
require_text 'bash scripts/ci/e2e_pull_request_reason.sh' \
  'the detector no longer reports its decision'

echo 'CLI end-to-end workflow trigger contract passed'
