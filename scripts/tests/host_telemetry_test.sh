#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
runner="$root/scripts/ci/run_with_host_telemetry.sh"
tmp="$(mktemp -d)"
wrapper_pid="" build_pid=""
trap 'if [[ -n "$wrapper_pid" ]]; then kill "$wrapper_pid" 2>/dev/null || true; fi; if [[ -n "$build_pid" ]]; then kill "$build_pid" 2>/dev/null || true; fi; rm -rf "$tmp"' EXIT
mkdir "$tmp/bin"

cat > "$tmp/bin/free" <<'SH'
#!/usr/bin/env bash
if [[ "${TELEMETRY_UNAVAILABLE:-}" == 1 ]]; then exit 1; fi
if [[ "${TELEMETRY_EMPTY:-}" == 1 ]]; then exit 0; fi
printf 'Mem: 16384 1024 15360\n'
SH
cat > "$tmp/bin/ps" <<'SH'
#!/usr/bin/env bash
if [[ "${TELEMETRY_UNAVAILABLE:-}" == 1 ]]; then exit 1; fi
if [[ "${TELEMETRY_EMPTY:-}" == 1 ]]; then exit 0; fi
printf '12345 5678 rustc\n'
SH
cat > "$tmp/bin/sudo" <<'SH'
#!/usr/bin/env bash
exit 1
SH
cat > "$tmp/bin/dmesg" <<'SH'
#!/usr/bin/env bash
if [[ "${TELEMETRY_UNAVAILABLE:-}" == 1 ]]; then exit 1; fi
if [[ "${TELEMETRY_EMPTY:-}" == 1 ]]; then exit 0; fi
printf 'kernel fixture: no kill observed\n'
SH
chmod +x "$tmp/bin/"*
export PATH="$tmp/bin:$PATH"

bash "$runner" "test build" true > "$tmp/success.log"
grep -q 'Host state during test build at .*Z' "$tmp/success.log"
grep -q 'Mem: 16384 1024 15360' "$tmp/success.log"
grep -q '12345.*5678.*rustc' "$tmp/success.log"
grep -q 'kernel fixture: no kill observed' "$tmp/success.log"

cat > "$tmp/fail.sh" <<'SH'
#!/usr/bin/env bash
exit 42
SH
status=0
bash "$runner" "test build" bash "$tmp/fail.sh" > "$tmp/failure.log" || status=$?
[[ "$status" == 42 ]]

# Missing diagnostics must be visible and must not replace a build failure.
status=0
TELEMETRY_UNAVAILABLE=1 bash "$runner" "test build" bash "$tmp/fail.sh" > "$tmp/unavailable.log" || status=$?
[[ "$status" == 42 ]]
grep -q 'free failed' "$tmp/unavailable.log"
grep -q 'process listing unavailable' "$tmp/unavailable.log"
grep -q 'kernel ring buffer not readable' "$tmp/unavailable.log"

TELEMETRY_EMPTY=1 bash "$runner" "test build" true > "$tmp/empty.log"
grep -q 'free failed or returned no memory reading' "$tmp/empty.log"
grep -q 'process listing unavailable' "$tmp/empty.log"
grep -q 'kernel ring buffer not readable' "$tmp/empty.log"

cat > "$tmp/signal.sh" <<'SH'
#!/usr/bin/env bash
kill -TERM "$$"
SH
status=0
bash "$runner" "test build" bash "$tmp/signal.sh" > "$tmp/signal.log" 2>&1 || status=$?
[[ "$status" == 143 ]]

# Drive the monitor's timer with a FIFO rather than real elapsed time. Each
# timer announces itself, so no assertion depends on a scheduler delay.
mkfifo "$tmp/events" "$tmp/ticks" "$tmp/build"
exec 7<> "$tmp/events"
exec 8<> "$tmp/ticks"
exec 9<> "$tmp/build"
cat > "$tmp/bin/sleep" <<'SH'
#!/usr/bin/env bash
[[ "$1" == 30 ]]
printf 'timer %s\n' "$PPID" >&7
read -r _ <&8
SH
cat > "$tmp/build.sh" <<'SH'
#!/usr/bin/env bash
printf 'build %s\n' "$$" >&7
read -r _ <&9
SH
chmod +x "$tmp/bin/sleep"

bash "$runner" "test build" bash "$tmp/build.sh" > "$tmp/live.log" 2>&1 &
wrapper_pid=$!
read -r kind pid <&7
if [[ "$kind" == build ]]; then
  build_pid=$pid
  read -r kind monitor_pid <&7
  [[ "$kind" == timer ]]
else
  [[ "$kind" == timer ]]
  monitor_pid=$pid
  read -r kind build_pid <&7
  [[ "$kind" == build ]]
fi
[[ "$(grep -c 'Host state during' "$tmp/live.log")" == 1 ]]
printf 'tick\n' >&8
read -r kind pid <&7
[[ "$kind" == timer && "$pid" == "$monitor_pid" ]]
[[ "$(grep -c 'Host state during' "$tmp/live.log")" == 2 ]]

# Killing the supervisor prevents EXIT reporting. The two earlier snapshots
# must already be in the log; neither relies on completion or a final upload.
kill -KILL "$wrapper_pid"
wait "$wrapper_pid" 2>/dev/null || true
wrapper_pid=""
[[ "$(grep -c 'Host state during' "$tmp/live.log")" == 2 ]]
kill "$build_pid"
build_pid=""
printf 'tick\n' >&8

echo "host telemetry: initial and periodic snapshots survive supervisor kill; build statuses preserved"
