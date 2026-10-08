#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
owner=${1:-$repo_root/scripts/ci/post_merge_tier.sh}
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
mkdir "$scratch/bin"
cat > "$scratch/bin/gh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
case "$*" in
  *compare*)
    [[ "${EMPTY_COMMITS:-false}" != true ]] || exit 0
    echo 3333333333333333333333333333333333333333
    echo 4444444444444444444444444444444444444444
    ;;
  *commits/3333*/pulls*) jq -n --arg body "${FIRST_BODY:-}" '[[{body: $body}]]' ;;
  *commits/4444*/pulls*) jq -n --arg body "${SECOND_BODY:-}" '[[{body: $body}]]' ;;
  *) exit 9 ;;
esac
SH
chmod +x "$scratch/bin/gh"
export PATH="$scratch/bin:$PATH" GITHUB_REPOSITORY=fixture/repo

check() {
  local event=$1 paths=$2 body=$3 expected=$4
  printf 'known_non_null=present\n' > "$scratch/output"
  jq -n --arg body "$body" '{pull_request: {body: $body}, merge_group: {base_sha: "1111111111111111111111111111111111111111", head_sha: "2222222222222222222222222222222222222222"}}' > "$scratch/event.json"
  EVENT_NAME="$event" FULL_SUITE_PATHS="$paths" GITHUB_EVENT_PATH="$scratch/event.json" \
    GITHUB_OUTPUT="$scratch/output" bash "$owner" > "$scratch/stdout"
  local actual
  actual=$(cat "$scratch/output")
  if [[ "$actual" != $'known_non_null=present\nactive='"$expected" ]]; then
    printf 'wrong post-merge tier decision: event=%s paths=%s body=%s expected=%s\n' \
      "$event" "$paths" "$body" "$expected" >&2
    exit 1
  fi
}

# A scoped change leaves the slow families to main.
check pull_request false '' true
check merge_group false '' true
# A full-suite path, or an unmeasured filter, runs them before landing.
check pull_request true '' false
check merge_group true '' false
check pull_request '' '' false
check merge_group 'maybe' '' false
# The declaration adds scope on a pull request.
check pull_request false $'Fixes it.\n\nCI-Scope: full\n' false
check pull_request false $'ci-scope:  full' false
FIRST_BODY='CI-Scope: full' check merge_group false '' false
SECOND_BODY='CI-Scope: full' check merge_group false '' false
# Main and every other event always run everything.
for event in push schedule workflow_dispatch pull_request_target; do
  check "$event" false '' false
done

jq -n '{pull_request: {body: "CI-Scope: harn-audit"}}' > "$scratch/event.json"
if EVENT_NAME=pull_request FULL_SUITE_PATHS=false GITHUB_EVENT_PATH="$scratch/event.json" \
  GITHUB_OUTPUT="$scratch/output" bash "$owner" > "$scratch/stdout" 2>&1; then
  echo 'an unknown declared scope was accepted' >&2
  exit 1
fi
if env -u EVENT_NAME GITHUB_OUTPUT="$scratch/output" bash "$owner" > "$scratch/stdout" 2>&1; then
  echo 'missing event was accepted' >&2
  exit 1
fi

# A decision that turns the tier on by default would skip proofs main and
# full-suite changes owe.
if [[ $# == 0 ]]; then
  sed 's/^active=false$/active=true/' "$owner" > "$scratch/overridden.sh"
  if bash "${BASH_SOURCE[0]}" "$scratch/overridden.sh" > "$scratch/negative.log" 2>&1; then
    echo 'main override escaped the owning shell controls' >&2
    exit 1
  fi
  if ! grep -q 'wrong post-merge tier decision' "$scratch/negative.log"; then
    echo 'negative control did not reach a decision check' >&2
    exit 1
  fi
  echo 'post-merge tier decision: event/path/declaration controls passed; main override refused'
fi
