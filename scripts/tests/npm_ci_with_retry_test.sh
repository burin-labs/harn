#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp_root="$(mktemp -d "${TMPDIR:-/tmp}/harn-npm-ci-retry-test.XXXXXX")"
trap 'rm -rf "$tmp_root"' EXIT
mkdir -p "$tmp_root/bin" "$tmp_root/package"

cat >"$tmp_root/bin/npm" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
count_file="${FAKE_NPM_STATE}/count"
count=0
[[ ! -f "$count_file" ]] || count="$(<"$count_file")"
count=$((count + 1))
printf '%s\n' "$count" >"$count_file"
case "${FAKE_NPM_MODE}" in
  transient-once)
    if [[ "$count" -eq 1 ]]; then
      echo 'npm error ECONNRESET: socket hang up' >&2
      exit 17
    fi
    ;;
  transient-always)
    echo 'npm error ETIMEDOUT downloading immutable dependency' >&2
    exit 18
    ;;
  transient-three)
    if [[ "$count" -le 3 ]]; then
      echo 'npm error ECONNRESET: sustained release asset edge reset' >&2
      exit 17
    fi
    ;;
  deterministic)
    echo 'npm error lifecycle script failed its test' >&2
    exit 19
    ;;
  postinstall-once|postinstall-always)
    if [[ "$FAKE_NPM_MODE" == postinstall-always || "$count" -eq 1 ]]; then
      echo 'npm error code 1' >&2
      echo 'npm error path /fixture/node_modules/tree-sitter-cli' >&2
      echo 'npm error command sh -c node install.js' >&2
      echo 'npm error Download failed' >&2
      printf 'npm error status: %s\n' "$FAKE_NPM_HTTP_STATUS" >&2
      exit 1
    fi
    ;;
esac
printf 'npm %s\n' "$*"
EOF
chmod +x "$tmp_root/bin/npm"

# Process substitutions are asynchronous. If the retry script sends its log
# through tee again, this delay makes its immediate classification reliably
# observe an empty file. Synchronous capture never invokes this shim.
cat >"$tmp_root/bin/tee" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'invoked\n' >>"${FAKE_NPM_STATE}/tee-invocations"
/bin/sleep 1
exec /usr/bin/tee "$@"
EOF
chmod +x "$tmp_root/bin/tee"

cat >"$tmp_root/bin/sleep" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$1" >>"${FAKE_NPM_STATE}/sleeps"
EOF
chmod +x "$tmp_root/bin/sleep"

run_case() {
  local name="$1" mode="$2" expected_status="$3" expected_calls="$4"
  local state="$tmp_root/$name"
  mkdir -p "$state"
  local status=0
  PATH="$tmp_root/bin:$PATH" FAKE_NPM_STATE="$state" FAKE_NPM_MODE="$mode" \
    FAKE_NPM_HTTP_STATUS="${5:-}" \
    "$repo_root/scripts/npm_ci_with_retry.sh" "$tmp_root/package" \
    >"$state/output" 2>&1 || status=$?
  [[ "$status" -eq "$expected_status" ]] || {
    echo "$name status: expected $expected_status, got $status" >&2
    cat "$state/output" >&2
    exit 1
  }
  [[ "$(<"$state/count")" -eq "$expected_calls" ]] || {
    echo "$name calls: expected $expected_calls, got $(<"$state/count")" >&2
    exit 1
  }
}

run_case transient-once transient-once 0 2
grep -Fq 'npm error ECONNRESET: socket hang up' "$tmp_root/transient-once/output"
grep -Fq 'npm ci' "$tmp_root/transient-once/output"
grep -Fq 'retrying in 5s (attempt 2/4)' "$tmp_root/transient-once/output"
[[ "$(<"$tmp_root/transient-once/sleeps")" == "5" ]]
run_case transient-three transient-three 0 4
[[ "$(paste -sd, "$tmp_root/transient-three/sleeps")" == "5,20,60" ]]
run_case transient-always transient-always 18 4
[[ "$(paste -sd, "$tmp_root/transient-always/sleeps")" == "5,20,60" ]]
run_case deterministic deterministic 19 1
[[ ! -e "$tmp_root/deterministic/sleeps" ]]
[[ ! -e "$tmp_root/transient-once/tee-invocations" ]]

# The real tree-sitter install hook exits with npm's generic code 1 and puts
# its HTTP status on a separate line, rather than emitting "HTTP 504".
for status in 504 429 500 503 599; do
  run_case "postinstall-$status" postinstall-once 0 2 "$status"
  [[ "$(<"$tmp_root/postinstall-$status/sleeps")" == 5 ]]
done
run_case postinstall-exhausted postinstall-always 1 4 504
[[ "$(paste -sd, "$tmp_root/postinstall-exhausted/sleeps")" == "5,20,60" ]]
for status in 400 401 403 404 5040 '504-test-assertion'; do
  run_case "postinstall-$status" postinstall-always 1 1 "$status"
  [[ ! -e "$tmp_root/postinstall-$status/sleeps" ]]
done

echo "npm ci bounded retry tests passed"
