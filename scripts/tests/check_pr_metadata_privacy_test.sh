#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
script="$root/scripts/check_pr_metadata_privacy.sh"
fixture_root="$(mktemp -d "${TMPDIR:-/tmp}/harn-pr-metadata-privacy.XXXXXX")"
trap 'rm -rf "$fixture_root"' EXIT

clean_event="$fixture_root/clean.json"
jq -n \
  --arg title 'Improve parser diagnostics' \
  --arg body 'Keeps the public contract host-neutral.' \
  '{pull_request: {title: $title, body: $body}}' >"$clean_event"
clean_output="$("$script" "$clean_event")"
if [[ "$clean_output" != 'pull-request metadata: fields=2 pending=0' ]]; then
  echo "error: clean metadata must report its non-empty measured scope" >&2
  printf '%s\n' "$clean_output" >&2
  exit 1
fi

product="burin"
product_event="$fixture_root/product.json"
jq -n \
  --arg title "Add ${product}-code compatibility" \
  --arg body 'Public details.' \
  '{pull_request: {title: $title, body: $body}}' >"$product_event"
captured="$fixture_root/captured.txt"
if "$script" "$product_event" >"$captured" 2>&1; then
  echo "error: a matched pull-request title must fail" >&2
  exit 1
fi
if grep -qiF "$product" "$captured"; then
  echo "error: the pull-request title match leaked into output" >&2
  exit 1
fi
if ! grep -q '^pull-request metadata: fields=2 pending=1$' "$captured"; then
  echo "error: matched metadata must report the pending count" >&2
  cat "$captured" >&2
  exit 1
fi
if ! grep -q '^pending fields: pull-request-title$' "$captured"; then
  echo "error: matched metadata must name the offending field" >&2
  cat "$captured" >&2
  exit 1
fi

body_event="$fixture_root/body.json"
jq -n \
  --arg title 'Improve parser diagnostics' \
  --arg body "Supports ${product}-evals paths." \
  '{pull_request: {title: $title, body: $body}}' >"$body_event"
if "$script" "$body_event" >"$captured" 2>&1; then
  echo "error: a matched pull-request body must fail" >&2
  exit 1
fi
if ! grep -q '^pending fields: pull-request-body$' "$captured"; then
  echo "error: a body match must name the offending field" >&2
  cat "$captured" >&2
  exit 1
fi

# A malformed or non-PR event must be unmeasured, not green.
non_pr_event="$fixture_root/non-pr.json"
jq -n '{merge_group: {head_sha: "abc"}}' >"$non_pr_event"
if "$script" "$non_pr_event" >"$captured" 2>&1; then
  echo "error: missing pull-request metadata must not pass vacuously" >&2
  exit 1
fi
if ! grep -q '^pull-request metadata: fields=0 pending=unmeasured$' "$captured"; then
  echo "error: an absent metadata scope must be visibly unmeasured" >&2
  cat "$captured" >&2
  exit 1
fi

empty_title_event="$fixture_root/empty-title.json"
jq -n '{pull_request: {title: "", body: ""}}' >"$empty_title_event"
if "$script" "$empty_title_event" >"$captured" 2>&1; then
  echo "error: an empty required title must not pass vacuously" >&2
  exit 1
fi
if ! grep -q '^pull-request metadata: fields=0 pending=unmeasured$' "$captured"; then
  echo "error: an empty title must be visibly unmeasured" >&2
  cat "$captured" >&2
  exit 1
fi

if ! grep -q 'check_pr_metadata_privacy.sh' "$root/.github/workflows/pr-gates.yml"; then
  echo "error: PR gates workflow must invoke the metadata privacy owner" >&2
  exit 1
fi
if ! grep -q 'github.event_path' "$root/.github/workflows/pr-gates.yml"; then
  echo "error: PR gates workflow must pass the event payload path" >&2
  exit 1
fi
if ! grep -q 'types: \[opened, reopened, synchronize, edited, labeled, unlabeled\]' \
  "$root/.github/workflows/pr-gates.yml"; then
  echo "error: editing public pull-request metadata must retrigger the gate" >&2
  exit 1
fi

echo "check_pr_metadata_privacy tests passed"
