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
if [[ ${GH_FAKE_PAGINATED:-} == 1 ]]; then
  case "$2" in
    *per_page=10\&page=1)
      printf '{"artifacts":['
      for ((i = 1; i <= 10; i++)); do
        ((i == 1)) || printf ','
        printf '{"id":%d,"name":"unrelated","expired":false,"workflow_run":{"head_branch":"main"}}' "$i"
      done
      printf ']}'
      exit 0
      ;;
    *per_page=10\&page=2)
      echo '{"artifacts":[{"id":42,"name":"prefix-newest","expired":false,"workflow_run":{"head_branch":"main"}}]}'
      exit 0
      ;;
    *artifacts/42/zip)
      cat "$GH_FAKE_ZIP"
      exit 0
      ;;
    *)
      echo "unexpected GitHub path: $2" >&2
      exit 1
      ;;
  esac
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

# A full first page must lead to a second small page and a real extraction.
printf 'main measurement\n' > "$root/measurement.txt"
(cd "$root" && zip -q "$root/measurement.zip" measurement.txt)
export GH_FAKE_ZIP="$root/measurement.zip"
rm -f "$GH_FAKE_COUNT"
out=$(GH_FAKE_PAGINATED=1 FAIL_FIRST=0 "$script" prefix- "$root/paged" 2>&1)
[[ "$out" == *"Restored main artifact 42"* ]]
[[ "$(cat "$root/paged/measurement.txt")" == "main measurement" ]]
[[ "$(cat "$GH_FAKE_COUNT")" == 3 ]]

echo "ci_fetch_latest_main_artifact_test: ok"
