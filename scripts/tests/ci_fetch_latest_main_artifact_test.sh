#!/usr/bin/env bash
# `fetch_latest_main_artifact.sh` must find main's newest measurement by exact
# artifact name, never through the unfiltered artifact listing GitHub times out
# on; must repeat a failed or empty GitHub read a bounded number of times, then
# fail the job; and must still report absence by name.
set -euo pipefail
script="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/ci/fetch_latest_main_artifact.sh"

root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT
mkdir -p "$root/bin"
# The fake gh fails its first $FAIL_FIRST calls (with an empty body and exit 0
# when GH_FAKE_EMPTY_OK=1, as a dropped response that gh did not flag), then
# answers by path, counting every call. main's newest commits are aaa, bbb,
# ccc; only ccc carries a usable artifact unless GH_FAKE_NONE=1.
cat > "$root/bin/gh" <<'GH'
#!/usr/bin/env bash
count=$(( $(cat "$GH_FAKE_COUNT" 2>/dev/null || echo 0) + 1 ))
echo "$count" > "$GH_FAKE_COUNT"
if (( count <= FAIL_FIRST )); then
  [[ ${GH_FAKE_EMPTY_OK:-} == 1 ]] && exit 0
  echo "unexpected end of JSON input" >&2
  exit 1
fi
case "$2" in
  *"/commits?sha=main&per_page=30")
    echo '[{"sha":"aaa"},{"sha":"bbb"},{"sha":"ccc"}]'
    ;;
  *"/actions/artifacts?name=prefix-aaa&per_page=10")
    echo '{"artifacts":[]}'
    ;;
  *"/actions/artifacts?name=prefix-bbb&per_page=10")
    echo '{"artifacts":[{"id":7,"expired":true,"workflow_run":{"head_branch":"main"}},{"id":8,"expired":false,"workflow_run":{"head_branch":"gh-readonly-queue/main/pr-1"}}]}'
    ;;
  *"/actions/artifacts?name=prefix-ccc&per_page=10")
    if [[ ${GH_FAKE_NONE:-} == 1 ]]; then
      echo '{"artifacts":[]}'
    else
      echo '{"artifacts":[{"id":42,"expired":false,"workflow_run":{"head_branch":"main"}}]}'
    fi
    ;;
  *"/actions/artifacts/42/zip")
    cat "$GH_FAKE_ZIP"
    ;;
  *"/actions/artifacts?per_page="*)
    # The real unfiltered listing: HTTP 500, empty body, after ~8 seconds.
    echo "gh: HTTP 500 (unexpected end of JSON input)" >&2
    exit 1
    ;;
  *)
    echo "unexpected GitHub path: $2" >&2
    exit 1
    ;;
esac
GH
chmod +x "$root/bin/gh"
export PATH="$root/bin:$PATH" GH_REPO=o/r GH_TOKEN=x FETCH_ARTIFACT_RETRY_SECONDS=0
export GH_FAKE_COUNT="$root/count"
printf 'main measurement\n' > "$root/measurement.txt"
(cd "$root" && zip -q "$root/measurement.zip" measurement.txt)
export GH_FAKE_ZIP="$root/measurement.zip"

# The newest main commit with an unexpired, main-headed artifact wins: aaa has
# none, bbb has only an expired one and a merge-queue one. One commit read,
# three name reads, one download.
rm -f "$GH_FAKE_COUNT"
out=$(FAIL_FIRST=0 "$script" prefix- "$root/found" 2>&1)
[[ "$out" == *"Restored main artifact 42"* ]]
[[ "$(cat "$root/found/measurement.txt")" == "main measurement" ]]
[[ "$(cat "$GH_FAKE_COUNT")" == 5 ]]

# One dropped response, then success.
rm -f "$GH_FAKE_COUNT"
out=$(FAIL_FIRST=1 "$script" prefix- "$root/retried" 2>&1)
[[ "$out" == *"retrying"* ]]
[[ "$out" == *"Restored main artifact 42"* ]]
[[ "$(cat "$GH_FAKE_COUNT")" == 6 ]]

# Nothing recorded is reported by name, not as a failure.
rm -f "$GH_FAKE_COUNT"
out=$(GH_FAKE_NONE=1 FAIL_FIRST=0 "$script" prefix- "$root/none" 2>&1)
[[ "$out" == *"No unexpired main artifact matching 'prefix-'"* ]]
[[ ! -e "$root/none/measurement.txt" ]]

# A read that keeps failing fails the job after three attempts.
rm -f "$GH_FAKE_COUNT"
if FAIL_FIRST=99 "$script" prefix- "$root/dest" >/dev/null 2>&1; then
  echo "a persistent API failure must fail the fetch" >&2
  exit 1
fi
[[ "$(cat "$GH_FAKE_COUNT")" == 3 ]]

# An empty body that gh reports as success must fail closed, never read as
# "no artifacts recorded".
rm -f "$GH_FAKE_COUNT"
if out=$(GH_FAKE_EMPTY_OK=1 FAIL_FIRST=99 "$script" prefix- "$root/empty" 2>&1); then
  echo "an empty API body must fail the fetch, got: $out" >&2
  exit 1
fi
[[ "$out" != *"No unexpired main artifact"* ]]
[[ "$(cat "$GH_FAKE_COUNT")" == 3 ]]

echo "ci_fetch_latest_main_artifact_test: ok"
