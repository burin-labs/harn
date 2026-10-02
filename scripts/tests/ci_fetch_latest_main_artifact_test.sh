#!/usr/bin/env bash
# `fetch_latest_main_artifact.sh` must repeat a failed GitHub read a bounded
# number of times, then fail the job, and must still report absence by name.
set -euo pipefail
script="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/ci/fetch_latest_main_artifact.sh"

root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT
mkdir -p "$root/bin"
# The fake gh fails its first $FAIL_FIRST calls, then answers with an empty
# artifact list, counting every call.
cat > "$root/bin/gh" <<'GH'
#!/usr/bin/env bash
count=$(( $(cat "$GH_FAKE_COUNT" 2>/dev/null || echo 0) + 1 ))
echo "$count" > "$GH_FAKE_COUNT"
if (( count <= FAIL_FIRST )); then
  echo "unexpected end of JSON input" >&2
  exit 1
fi
echo '{"artifacts": []}'
GH
chmod +x "$root/bin/gh"
export PATH="$root/bin:$PATH" GH_REPO=o/r GH_TOKEN=x FETCH_ARTIFACT_RETRY_SECONDS=0
export GH_FAKE_COUNT="$root/count"

# One dropped response, then success: absence is still reported by name.
rm -f "$GH_FAKE_COUNT"
out=$(FAIL_FIRST=1 "$script" prefix- "$root/dest" 2>&1)
[[ "$out" == *"retrying"* ]]
[[ "$out" == *"No unexpired main artifact matching 'prefix-'"* ]]
[[ "$(cat "$GH_FAKE_COUNT")" == 2 ]]

# A read that keeps failing fails the job after three attempts.
rm -f "$GH_FAKE_COUNT"
if FAIL_FIRST=99 "$script" prefix- "$root/dest" >/dev/null 2>&1; then
  echo "a persistent API failure must fail the fetch" >&2
  exit 1
fi
[[ "$(cat "$GH_FAKE_COUNT")" == 3 ]]

echo "ci_fetch_latest_main_artifact_test: ok"
