#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
: "${HARN_BIN:?run this integration through make test-pr-gate-post-warm-integrations}"
if [[ ! -x "$HARN_BIN" ]]; then
  echo "HARN_BIN is not executable: $HARN_BIN" >&2
  exit 1
fi

tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

real_harn_bin="$HARN_BIN"

fake_repo="$tmp_root/repo with spaces"
mkdir -p "$fake_repo/scripts"
git -C "$fake_repo" init --quiet
fake_repo="$(cd "$fake_repo" && pwd -P)"

# Records one line per invocation so the test can tell the blocking bootstrap
# phase apart from the background warm, and holds the warm open while
# CLAUDE_DEV_SETUP_TEST_WARM_GATE exists so single-flight is observable.
cat > "$fake_repo/scripts/dev_setup.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
profile="${HARN_DEV_SETUP_PROFILE:-full}"
printf 'profile=%s pwd=%s target=%s\n' \
  "$profile" "$PWD" "${HARN_DEV_TARGET_WORKTREE_PATH-}" >> "$CLAUDE_DEV_SETUP_TEST_RECORD"
if [[ "$profile" == "bootstrap" ]]; then
  exit "${CLAUDE_DEV_SETUP_TEST_BOOTSTRAP_STATUS:-0}"
fi
if [[ -n "${CLAUDE_DEV_SETUP_TEST_WARM_GATE:-}" ]]; then
  while [[ -e "$CLAUDE_DEV_SETUP_TEST_WARM_GATE" ]]; do sleep 0.05; done
fi
exit "${CLAUDE_DEV_SETUP_TEST_WARM_STATUS:-0}"
SH
chmod +x "$fake_repo/scripts/dev_setup.sh"

record="$tmp_root/setup-record.txt"
output="$tmp_root/hook-output.json"
state_dir="$fake_repo/.claude/dev-setup"
lock_dir="$state_dir/warm.lock"
: > "$record"

hook_message() {
  "$real_harn_bin" run -e 'pipeline main(harness: Harness, task: unknown) { const parsed = json_parse(harness.stdio.read_stdin() ?? ""); harness.stdio.println(parsed?.hookSpecificOutput?.additionalContext ?? "") }' < "$1"
}

# Bounded wait: the warm phase is a detached process, so there is no handle to
# join. Keep the ceiling well above the work these fakes do.
wait_for() {
  local description="$1" deadline=$((SECONDS + 30))
  shift
  while ! "$@"; do
    if (( SECONDS > deadline )); then
      echo "timed out waiting for $description" >&2
      cat "$record" >&2
      exit 1
    fi
    sleep 0.1
  done
}

warm_runs() { grep -c '^profile=full ' "$record" || true; }

# --- Session start returns without waiting on the expensive phases ----------

warm_gate="$tmp_root/warm-gate"
: > "$warm_gate"

CLAUDE_DEV_SETUP_TEST_RECORD="$record" \
  CLAUDE_DEV_SETUP_TEST_WARM_GATE="$warm_gate" \
  HARN_BIN="$real_harn_bin" \
  "$repo_root/scripts/claude-dev-setup-once.sh" > "$output" <<JSON
{"cwd":"$fake_repo"}
JSON

if ! grep -Fxq "profile=bootstrap pwd=$fake_repo target=$fake_repo" "$record"; then
  echo "hook did not run the bootstrap profile against the cwd parsed by Harn" >&2
  cat "$record" >&2
  exit 1
fi

parsed_message="$(hook_message "$output")"
if [[ "$parsed_message" != "Worktree configured in "* ]]; then
  echo "hook did not emit a SessionStart context JSON object for the fast path" >&2
  cat "$output" >&2
  exit 1
fi

# The hook returned; the full profile is still gated open in the background.
wait_for "the background warm to start" test -d "$lock_dir"
if [[ "$(warm_runs)" != "1" ]]; then
  echo "expected exactly one background warm run, got $(warm_runs)" >&2
  cat "$record" >&2
  exit 1
fi
if compgen -G "$state_dir/*.stamp" > /dev/null; then
  echo "hook stamped the fingerprint before the warm finished" >&2
  exit 1
fi

# --- A second session does not start a competing warm ----------------------

second_output="$tmp_root/hook-output-2.json"
CLAUDE_DEV_SETUP_TEST_RECORD="$record" \
  CLAUDE_DEV_SETUP_TEST_WARM_GATE="$warm_gate" \
  HARN_BIN="$real_harn_bin" \
  "$repo_root/scripts/claude-dev-setup-once.sh" > "$second_output" <<JSON
{"cwd":"$fake_repo"}
JSON

if [[ "$(warm_runs)" != "1" ]]; then
  echo "a second session started a competing warm run" >&2
  cat "$record" >&2
  exit 1
fi
second_message="$(hook_message "$second_output")"
if [[ "$second_message" != *"another session"* ]]; then
  echo "hook did not report the in-flight warm to the second session" >&2
  cat "$second_output" >&2
  exit 1
fi

# --- Finishing the warm stamps the fingerprint and frees the lock ----------

rm -f "$warm_gate"
wait_for "the warm to stamp the fingerprint" \
  bash -c 'compgen -G "$1/*.stamp" > /dev/null' _ "$state_dir"
wait_for "the warm lock to be released" test '!' -d "$lock_dir"

if [[ ! -L "$state_dir/latest.log" ]]; then
  echo "warm did not publish a latest.log symlink" >&2
  exit 1
fi

# A session on an unchanged checkout now short-circuits entirely.
before=$(wc -l < "$record")
CLAUDE_DEV_SETUP_TEST_RECORD="$record" \
  HARN_BIN="$real_harn_bin" \
  "$repo_root/scripts/claude-dev-setup-once.sh" >/dev/null <<JSON
{"cwd":"$fake_repo"}
JSON
if [[ "$(wc -l < "$record")" != "$before" ]]; then
  echo "stamped checkout re-ran setup" >&2
  cat "$record" >&2
  exit 1
fi

# --- A failed bootstrap reports the failure and warms nothing --------------

rm -rf "$state_dir"
: > "$record"
fail_output="$tmp_root/hook-output-fail.json"
CLAUDE_DEV_SETUP_TEST_RECORD="$record" \
  CLAUDE_DEV_SETUP_TEST_BOOTSTRAP_STATUS=3 \
  HARN_BIN="$real_harn_bin" \
  "$repo_root/scripts/claude-dev-setup-once.sh" > "$fail_output" <<JSON
{"cwd":"$fake_repo"}
JSON

fail_message="$(hook_message "$fail_output")"
if [[ "$fail_message" != *"could not configure"* ]]; then
  echo "hook did not report a failed bootstrap" >&2
  cat "$fail_output" >&2
  exit 1
fi
if [[ "$(warm_runs)" != "0" ]]; then
  echo "hook warmed dependencies after the bootstrap failed" >&2
  cat "$record" >&2
  exit 1
fi
if compgen -G "$state_dir/*.stamp" > /dev/null; then
  echo "hook stamped the fingerprint after the bootstrap failed" >&2
  exit 1
fi

# --- Malformed hook input still falls back to CLAUDE_PROJECT_DIR -----------

rm -rf "$state_dir"
: > "$record"
CLAUDE_PROJECT_DIR="$fake_repo" \
  CLAUDE_DEV_SETUP_TEST_RECORD="$record" \
  HARN_BIN="$real_harn_bin" \
  "$repo_root/scripts/claude-dev-setup-once.sh" >/dev/null <<'JSON'
not-json
JSON

if ! grep -Fxq "profile=bootstrap pwd=$fake_repo target=$fake_repo" "$record"; then
  echo "hook did not fall back to CLAUDE_PROJECT_DIR for malformed input" >&2
  cat "$record" >&2
  exit 1
fi
wait_for "the fallback warm to finish" test '!' -d "$lock_dir"

echo "claude_dev_setup_once_test: ok"
