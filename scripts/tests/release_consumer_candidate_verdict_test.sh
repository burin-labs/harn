#!/usr/bin/env bash
# Exercise the actual aggregate check, including release PRs with no archives.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
awk '/      - name: Require every candidate job to pass/{found=1} found && /        run: \|/{body=1;next} body && /^  [^ ]/{exit} body && /^      - /{exit} body{print substr($0,11)}' \
  "$root/.github/workflows/build-release-binaries.yml" > "$tmp/verdict.sh"
[[ -s "$tmp/verdict.sh" ]] || { echo 'missing aggregate verdict' >&2; exit 1; }
results='{"setup":{"result":"success"},"prepare_cli_aot":{"result":"success"},"build":{"result":"success"},"collect_candidate_manifest":{"result":"success"},"release-residual-audit":{"result":"success"},"release-smoke":{"result":"success"}}'
sha=1111111111111111111111111111111111111111
for mode in candidate none; do
  for consumer in skipped failure cancelled '' success; do
    status=0
    env SETUP_RESULT=success BUILD_MODE="$mode" CANDIDATE_PURPOSE=release \
      REHEARSAL_SOURCE_SHA="$sha" CONSUMER_RESULT="$consumer" RESULTS="$results" \
      bash "$tmp/verdict.sh" > "$tmp/result.log" 2>&1 || status=$?
    if [[ "$consumer" == success ]]; then
      [[ "$status" == 0 ]] || { cat "$tmp/result.log"; exit 1; }
    else
      [[ "$status" != 0 ]] || { echo "accepted consumer $consumer in $mode"; exit 1; }
      grep -Fq 'Consumer release rehearsal' "$tmp/result.log"
    fi
  done
done
if env SETUP_RESULT=success BUILD_MODE=candidate CANDIDATE_PURPOSE=release \
  REHEARSAL_SOURCE_SHA='' CONSUMER_RESULT=skipped RESULTS="$results" \
  bash "$tmp/verdict.sh" > "$tmp/result.log" 2>&1; then
  echo 'accepted release with no rehearsal identity'; exit 1
fi
# Source qualification and ordinary PRs intentionally dispatch no consumer.
for mode in candidate none; do
  env SETUP_RESULT=success BUILD_MODE="$mode" CANDIDATE_PURPOSE=source \
    REHEARSAL_SOURCE_SHA='' CONSUMER_RESULT=skipped RESULTS="$results" \
    bash "$tmp/verdict.sh" > "$tmp/result.log" 2>&1
done
echo 'release_consumer_candidate_verdict_test: ok'
