#!/usr/bin/env bash
# Says whether a pull request runs the slow E2E suite, and why.
#
# The suite is opt-in by the `e2e` label for most changes, because it is slow.
# Two surfaces are their own reason to run it: the suite's own tests, and the
# ACP adapter they exercise end to end. A change to the ACP `session/new`
# contract edited a test in this suite without the label, so its pull request
# never executed the suite and main stayed red for thirteen hours while every
# later pull request was green by construction.
#
# The decision is printed and written to the job summary, not only emitted as
# an output. A gate that quietly answers "no" reads exactly like a gate that
# answered "yes" and found nothing, and the whole defect this closes is a
# suite that did not run and said nothing about it. Listing the touched paths
# is what makes a wrong filter visible instead of merely silent.
set -euo pipefail

TOUCHED=${TOUCHED:-false}
TOUCHED_FILES=${TOUCHED_FILES:-}
LABELLED=${LABELLED:-false}

reasons=()
if [[ "${LABELLED}" == "true" ]]; then
  reasons+=("the \`e2e\` label is on this pull request")
fi
if [[ "${TOUCHED}" == "true" ]]; then
  reasons+=("the diff touches the E2E suite or the ACP adapter")
fi

if [[ ${#reasons[@]} -gt 0 ]]; then
  should_run=true
  verdict="RUNNING the slow E2E suite"
else
  should_run=false
  verdict="NOT running the slow E2E suite"
fi

{
  echo "### Slow E2E suite: ${verdict}"
  echo
  if [[ ${#reasons[@]} -gt 0 ]]; then
    echo "Because:"
    for reason in "${reasons[@]}"; do
      echo "- ${reason}"
    done
  else
    echo "No watched path was changed and the \`e2e\` label is absent. Add the"
    echo "label to run it anyway."
  fi
  echo
  echo "Watched paths: \`.github/workflows/e2e.yml\`,"
  echo "\`crates/harn-cli/tests/harn_cli_e2e/**\`,"
  echo "\`crates/harn-serve/src/adapters/acp/**\`."
  echo
  if [[ -n "${TOUCHED_FILES}" ]]; then
    echo "Watched files in this diff: ${TOUCHED_FILES}"
  else
    echo "Watched files in this diff: none."
  fi
} | tee -a "${GITHUB_STEP_SUMMARY:-/dev/null}"

echo "E2E_PULL_REQUEST_DECISION should_run=${should_run} labelled=${LABELLED} touched=${TOUCHED}"
echo "should_run=${should_run}" >> "${GITHUB_OUTPUT:-/dev/null}"
