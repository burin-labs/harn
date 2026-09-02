#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
fixture_root="$(mktemp -d "${TMPDIR:-/tmp}/harn-pr-metadata-privacy.XXXXXX")"
trap 'rm -rf "$fixture_root"' EXIT
repo="$fixture_root/repo"
mkdir -p "$repo/scripts"
cp "$root/scripts/check_pr_metadata_privacy.sh" "$repo/scripts/"
cp "$root/scripts/check_public_product_names.sh" "$repo/scripts/"
cp "$root/scripts/scan_hashed_denylist.mjs" "$repo/scripts/"

denied_token="synthetic.private"
printf '%s' "$denied_token" | shasum -a 256 | awk '{print $1}' \
  >"$repo/scripts/consumer-host-denylist.sha256"
git -C "$repo" init -q -b main
git -C "$repo" config user.name 'Metadata Test'
git -C "$repo" config user.email 'metadata@example.invalid'
git -C "$repo" add scripts
git -C "$repo" commit -q -m 'Base metadata scanner'
base_sha="$(git -C "$repo" rev-parse HEAD)"

git -C "$repo" switch -q -c feature
printf 'clean\n' >"$repo/feature.txt"
git -C "$repo" add feature.txt
git -C "$repo" commit -q -m 'Improve parser diagnostics' -m 'Keeps the public contract host-neutral.'
clean_head="$(git -C "$repo" rev-parse HEAD)"

# The event's base tip may advance after the feature branch diverges. The PR
# range is still the merge-base..head set, not an ancestry failure.
git -C "$repo" switch -q main
printf 'new base\n' >"$repo/base-advance.txt"
git -C "$repo" add base-advance.txt
git -C "$repo" commit -q -m 'Advance the base branch'
current_base="$(git -C "$repo" rev-parse HEAD)"
git -C "$repo" switch -q feature

clean_event="$fixture_root/clean.json"
jq -n \
  --arg title 'Improve parser diagnostics' \
  --arg body 'Keeps the public contract host-neutral.' \
  --arg base "$current_base" \
  --arg head "$clean_head" \
  '{pull_request: {title: $title, body: $body, base: {sha: $base}, head: {sha: $head}}}' \
  >"$clean_event"
clean_output="$("$repo/scripts/check_pr_metadata_privacy.sh" "$clean_event")"
if [[ "$clean_output" != 'public metadata: sources=3 commits=1 pending=0' ]]; then
  echo "error: clean metadata must report every measured source and commit" >&2
  printf '%s\n' "$clean_output" >&2
  exit 1
fi

# Existing title and body coverage stays on the same normalized source stream.
product="burin"
title_event="$fixture_root/title.json"
jq -n \
  --arg title "Add ${product}-code compatibility" \
  --arg body 'Public details.' \
  --arg base "$current_base" \
  --arg head "$clean_head" \
  '{pull_request: {title: $title, body: $body, base: {sha: $base}, head: {sha: $head}}}' \
  >"$title_event"
captured="$fixture_root/captured.txt"
if "$repo/scripts/check_pr_metadata_privacy.sh" "$title_event" >"$captured" 2>&1; then
  echo "error: a matched pull-request title must fail" >&2
  exit 1
fi
if grep -qiF "$product" "$captured"; then
  echo "error: the pull-request title match leaked into output" >&2
  exit 1
fi
if ! grep -q '^pending sources: pull-request-title$' "$captured"; then
  echo "error: a title match must identify the normalized title source" >&2
  cat "$captured" >&2
  exit 1
fi

body_event="$fixture_root/body.json"
jq -n \
  --arg title 'Improve parser diagnostics' \
  --arg body "Supports ${product}-evals paths." \
  --arg base "$current_base" \
  --arg head "$clean_head" \
  '{pull_request: {title: $title, body: $body, base: {sha: $base}, head: {sha: $head}}}' \
  >"$body_event"
if "$repo/scripts/check_pr_metadata_privacy.sh" "$body_event" >"$captured" 2>&1; then
  echo "error: a matched pull-request body must fail" >&2
  exit 1
fi
if ! grep -q '^pending sources: pull-request-body$' "$captured"; then
  echo "error: a body match must identify the normalized body source" >&2
  cat "$captured" >&2
  exit 1
fi

# The hash-only arm must inspect the commit body, identify it without exposing
# the denied token, and count the source as pending.
printf 'denied\n' >>"$repo/feature.txt"
git -C "$repo" add feature.txt
git -C "$repo" commit -q -m 'Keep public wording neutral' -m "Internal route: $denied_token"
denied_head="$(git -C "$repo" rev-parse HEAD)"
denied_event="$fixture_root/denied.json"
jq -n \
  --arg title 'Keep public wording neutral' \
  --arg body 'No private details here.' \
  --arg base "$clean_head" \
  --arg head "$denied_head" \
  '{pull_request: {title: $title, body: $body, base: {sha: $base}, head: {sha: $head}}}' \
  >"$denied_event"
if "$repo/scripts/check_pr_metadata_privacy.sh" "$denied_event" >"$captured" 2>&1; then
  echo "error: a hashed-denylist match in a commit body must fail" >&2
  exit 1
fi
if grep -qF "$denied_token" "$captured"; then
  echo "error: a denied commit token leaked into public output" >&2
  exit 1
fi
if ! grep -q "^public metadata: sources=3 commits=1 pending=1$" "$captured"; then
  echo "error: denied commit metadata must report the measured shape" >&2
  cat "$captured" >&2
  exit 1
fi
if ! grep -q "^pending sources: commit/$denied_head/message$" "$captured"; then
  echo "error: denied commit metadata must identify the exact commit source" >&2
  cat "$captured" >&2
  exit 1
fi
if ! grep -Eq "^commit/$denied_head/message:3: sha256:[0-9a-f]{12}$" "$captured"; then
  echo "error: denied commit output must contain only identifier, line, and digest" >&2
  cat "$captured" >&2
  exit 1
fi

printf 'denied subject\n' >>"$repo/feature.txt"
git -C "$repo" add feature.txt
git -C "$repo" commit -q -m "Reject $denied_token metadata" -m 'Public body remains neutral.'
denied_subject_head="$(git -C "$repo" rev-parse HEAD)"
jq -n \
  --arg title 'Commit subject control' \
  --arg body 'No private details here.' \
  --arg base "$denied_head" \
  --arg head "$denied_subject_head" \
  '{pull_request: {title: $title, body: $body, base: {sha: $base}, head: {sha: $head}}}' \
  >"$fixture_root/denied-subject.json"
if "$repo/scripts/check_pr_metadata_privacy.sh" "$fixture_root/denied-subject.json" \
  >"$captured" 2>&1; then
  echo "error: a hashed-denylist match in a commit subject must fail" >&2
  exit 1
fi
if ! grep -Eq "^commit/$denied_subject_head/message:1: sha256:[0-9a-f]{12}$" \
  "$captured"; then
  echo "error: commit subjects must retain exact identifier and line attribution" >&2
  cat "$captured" >&2
  exit 1
fi

# Merge-group events scan their exact base..head range without inventing absent
# title/body fields.
merge_event="$fixture_root/merge-group.json"
jq -n --arg base "$base_sha" --arg head "$clean_head" \
  '{merge_group: {base_sha: $base, head_sha: $head}}' >"$merge_event"
merge_output="$("$repo/scripts/check_pr_metadata_privacy.sh" "$merge_event")"
if [[ "$merge_output" != 'public metadata: sources=1 commits=1 pending=0' ]]; then
  echo "error: merge-group metadata must scan its measured commit range" >&2
  printf '%s\n' "$merge_output" >&2
  exit 1
fi

# A shallow checkout is valid when the complete explicit range is present.
shallow_repo="$fixture_root/shallow"
git clone -q --no-local --depth 2 --branch feature "file://$repo" "$shallow_repo"
cp "$repo/scripts/consumer-host-denylist.sha256" \
  "$shallow_repo/scripts/consumer-host-denylist.sha256"
shallow_base="$(git -C "$shallow_repo" rev-parse HEAD^)"
shallow_head="$(git -C "$shallow_repo" rev-parse HEAD)"
jq -n \
  --arg title 'Shallow history control' \
  --arg body 'Commit range remains explicit.' \
  --arg base "$shallow_base" \
  --arg head "$shallow_head" \
  '{pull_request: {title: $title, body: $body, base: {sha: $base}, head: {sha: $head}}}' \
  >"$fixture_root/shallow.json"
if "$shallow_repo/scripts/check_pr_metadata_privacy.sh" "$fixture_root/shallow.json" \
  >"$captured" 2>&1; then
  echo "error: shallow positive control must still detect the denied commit body" >&2
  exit 1
fi
if ! grep -q "^pending sources: commit/$shallow_head/message$" "$captured"; then
  echo "error: shallow history must retain exact commit attribution" >&2
  cat "$captured" >&2
  exit 1
fi

# Missing range objects are unmeasured, never clean.
missing_event="$fixture_root/missing-range.json"
jq -n \
  --arg title 'Unavailable history' \
  --arg body 'Must fail closed.' \
  --arg base '0000000000000000000000000000000000000000' \
  --arg head "$clean_head" \
  '{pull_request: {title: $title, body: $body, base: {sha: $base}, head: {sha: $head}}}' \
  >"$missing_event"
if "$repo/scripts/check_pr_metadata_privacy.sh" "$missing_event" >"$captured" 2>&1; then
  echo "error: unavailable commit enumeration must not pass vacuously" >&2
  exit 1
fi
if ! grep -q '^public metadata: sources=2 commits=unmeasured pending=commit-enumeration$' \
  "$captured"; then
  echo "error: unavailable commit enumeration must be visibly pending" >&2
  cat "$captured" >&2
  exit 1
fi

# A malformed event is also visibly unmeasured.
non_pr_event="$fixture_root/non-pr.json"
jq -n '{repository: {name: "harn"}}' >"$non_pr_event"
if "$repo/scripts/check_pr_metadata_privacy.sh" "$non_pr_event" >"$captured" 2>&1; then
  echo "error: missing public metadata must not pass vacuously" >&2
  exit 1
fi
if ! grep -q '^public metadata: sources=0 commits=unmeasured pending=commit-enumeration$' \
  "$captured"; then
  echo "error: an absent metadata scope must be visibly unmeasured" >&2
  cat "$captured" >&2
  exit 1
fi

workflow="$root/.github/workflows/pr-gates.yml"
if ! grep -q 'check_pr_metadata_privacy.sh' "$workflow"; then
  echo "error: PR gates workflow must invoke the metadata privacy owner" >&2
  exit 1
fi
if ! grep -A12 'public-metadata-privacy:' "$workflow" | grep -q 'fetch-depth: 0'; then
  echo "error: metadata privacy checkout must include the exact commit range" >&2
  exit 1
fi
if grep -A20 'public-metadata-privacy:' "$workflow" | grep -q 'Skip merge-group replay'; then
  echo "error: merge-group metadata must not bypass the scanner" >&2
  exit 1
fi
if ! grep -q 'types: \[opened, reopened, synchronize, edited, labeled, unlabeled\]' \
  "$workflow"; then
  echo "error: editing public pull-request metadata must retrigger the gate" >&2
  exit 1
fi

echo "check_pr_metadata_privacy tests passed"
