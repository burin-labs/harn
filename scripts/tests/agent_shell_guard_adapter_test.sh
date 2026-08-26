#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
if [[ -z "${HARN_BIN:-}" || ! -x "$HARN_BIN" ]]; then
  echo "agent shell guard adapter test requires an executable HARN_BIN" >&2
  exit 1
fi

fixture_root="$(cd "$(mktemp -d)" && pwd -P)"
order_root="$(cd "$(mktemp -d)" && pwd -P)"
trap 'rm -rf "$fixture_root" "$order_root"' EXIT
mkdir -p "$fixture_root/scripts"
cp "$repo_root/scripts/agent-shell-guard.sh" "$fixture_root/scripts/"
cp "$repo_root/scripts/agent_shell_guard.harn" "$fixture_root/scripts/"
cp "$repo_root/scripts/agent_shell_guard_policy.harn" "$fixture_root/scripts/"

cat >"$fixture_root/harn.toml" <<'TOML'
[package]
name = "broken-neighbor-fixture"

[exports]
trigger_handlers = "trigger_handlers.harn"

[[triggers]]
id = "broken-neighbor"
kind = "cron"
provider = "cron"
schedule = "* * * * *"
match = { events = ["cron.tick"] }
handler = "trigger_handlers::on_tick"
TOML

cat >"$fixture_root/trigger_handlers.harn" <<'HARN'
let eager_failure = 1 / 0

pub fn on_tick(_event) -> nil {
  return nil
}
HARN

payload='{"tool_name":"Bash","tool_input":{"command":"cargo check"}}'
if printf '%s' "$payload" \
  | "$HARN_BIN" run --eager-project-handlers \
    "$fixture_root/scripts/agent_shell_guard.harn" \
    >"$fixture_root/eager.out" 2>"$fixture_root/eager.err"; then
  echo "broken neighboring trigger unexpectedly initialized successfully" >&2
  exit 1
fi
if ! grep -Fq "failed to install manifest triggers" "$fixture_root/eager.err"; then
  echo "fixture did not prove the eager project-handler failure" >&2
  cat "$fixture_root/eager.err" >&2
  exit 1
fi

blocked="$(
  printf '%s' "$payload" \
    | HARN_BIN="$HARN_BIN" "$fixture_root/scripts/agent-shell-guard.sh"
)"
expected_make_reason="Run \`make check\` instead."
if [[ "$blocked" != *'"permissionDecision":"deny"'* ]] \
  || [[ "$blocked" != *"$expected_make_reason"* ]]; then
  echo "adapter did not preserve the raw Cargo denial beside a broken trigger" >&2
  printf '%s\n' "$blocked" >&2
  exit 1
fi

allowed="$(
  printf '%s' '{"tool_name":"Bash","tool_input":{"command":"make check"}}' \
    | HARN_BIN="$HARN_BIN" "$fixture_root/scripts/agent-shell-guard.sh"
)"
if [[ -n "$allowed" ]]; then
  echo "adapter emitted a decision for the supported Make target" >&2
  printf '%s\n' "$allowed" >&2
  exit 1
fi

quoted_pipeline="$({
  printf '%s' \
    '{"tool_name":"Bash","tool_input":{"command":"ps -axo command | rg '\''rustc .*harn_vm|cargo build --locked'\'' | head -n 20"}}'
} | HARN_BIN="$HARN_BIN" "$fixture_root/scripts/agent-shell-guard.sh")"
if [[ -n "$quoted_pipeline" ]]; then
  echo "adapter treated a quoted search pattern as a command" >&2
  printf '%s\n' "$quoted_pipeline" >&2
  exit 1
fi

mkdir -p "$fixture_root/model-probe"
cp "$repo_root/scripts/agent-shell-guard.sh" "$fixture_root/model-probe/"
cat >"$fixture_root/model-probe/agent_shell_guard.harn" <<'HARN'
fn main(harness: Harness) {
  const response = harness.llm.call("guard capability probe", nil, {
    provider: "ollama",
    model: "guard-never-runs",
  })
  harness.stdio.println(response)
}
HARN

printf '%s' '{}' \
  | HARN_BIN="$HARN_BIN" AGENT_SHELL_GUARD_DEBUG=1 \
    "$fixture_root/model-probe/agent-shell-guard.sh" \
    >"$fixture_root/model-probe.out" 2>"$fixture_root/model-probe.err"
if ! grep -Fq "llm_call' is not permitted" "$fixture_root/model-probe.err"; then
  echo "adapter capability policy did not reject a model call" >&2
  cat "$fixture_root/model-probe.err" >&2
  exit 1
fi

# A hook holds the agent's shell call open while it runs. If the guard can only
# be stopped by the harness timeout, a slow policy costs the agent the entire
# hook budget and still yields no verdict. Prove the adapter enforces its own
# deadline, releases the hook, and denies the command when the policy has not
# established that it is safe.
cat >"$fixture_root/hanging-harn" <<'STUB'
#!/usr/bin/env bash
trap 'exit 0' TERM
(trap '' TERM; while :; do sleep 1; done) &
printf '%s\n' "$!" >"$GUARD_CHILD_PID_FILE"
printf '%s\n' '{"partial":"must-not-escape"}'
wait
STUB
chmod +x "$fixture_root/hanging-harn"

deadline_start="$(date +%s)"
hung_output="$(
  printf '%s' "$payload" \
    | GUARD_CHILD_PID_FILE="$fixture_root/hanging-child.pid" \
      HARN_BIN="$fixture_root/hanging-harn" \
      AGENT_SHELL_GUARD_DEADLINE_SECONDS=1 \
      AGENT_SHELL_GUARD_KILL_GRACE_SECONDS=1 \
      "$fixture_root/scripts/agent-shell-guard.sh"
)"
deadline_elapsed="$(( $(date +%s) - deadline_start ))"
if [[ "$hung_output" != *'"permissionDecision":"deny"'* ]] \
  || [[ "$hung_output" != *'timed out'* ]]; then
  echo "adapter did not deny after the policy deadline" >&2
  printf '%s\n' "$hung_output" >&2
  exit 1
fi
if [[ "$hung_output" == *'must-not-escape'* ]]; then
  echo "adapter forwarded partial output from a timed-out policy" >&2
  printf '%s\n' "$hung_output" >&2
  exit 1
fi
if (( deadline_elapsed >= 10 )); then
  echo "adapter waited ${deadline_elapsed}s on a hanging policy past its bounded deadline" >&2
  exit 1
fi
hanging_child_pid="$(cat "$fixture_root/hanging-child.pid")"
if kill -0 "$hanging_child_pid" 2>/dev/null; then
  echo "adapter left a TERM-ignoring policy descendant running: $hanging_child_pid" >&2
  exit 1
fi

# Timeout and signal-shaped exits mean the policy produced no trustworthy
# decision, so all three statuses deny. Other interpreter failures remain
# fail-open so a broken local runtime cannot lock every shell call.
cat >"$fixture_root/status-harn" <<'STUB'
#!/usr/bin/env bash
if [[ "${GUARD_PARTIAL:-0}" == "1" ]]; then
  printf '%s\n' '{"partial":"must-not-escape"}'
fi
exit "${GUARD_STATUS:?GUARD_STATUS is required}"
STUB
chmod +x "$fixture_root/status-harn"
for timeout_status in 124 137 143; do
  timeout_output="$(
    printf '%s' "$payload" \
      | GUARD_STATUS="$timeout_status" HARN_BIN="$fixture_root/status-harn" \
        "$fixture_root/scripts/agent-shell-guard.sh"
  )"
  if [[ "$timeout_output" != *'"permissionDecision":"deny"'* ]]; then
    echo "adapter did not deny policy status $timeout_status" >&2
    printf '%s\n' "$timeout_output" >&2
    exit 1
  fi
done

crash_output="$(
  printf '%s' "$payload" \
    | GUARD_PARTIAL=1 GUARD_STATUS=9 HARN_BIN="$fixture_root/status-harn" \
      "$fixture_root/scripts/agent-shell-guard.sh"
)"
if [[ -n "$crash_output" ]]; then
  echo "adapter did not fail open after an interpreter crash" >&2
  printf '%s\n' "$crash_output" >&2
  exit 1
fi

unavailable_output="$(
  printf '%s' "$payload" \
    | env -u HARN_BIN PATH=/usr/bin:/bin \
      AGENT_SHELL_GUARD_HARN_BIN="$fixture_root/missing-harn" \
      "$fixture_root/scripts/agent-shell-guard.sh"
)"
if [[ -n "$unavailable_output" ]]; then
  echo "adapter did not fail open when no interpreter was available" >&2
  printf '%s\n' "$unavailable_output" >&2
  exit 1
fi

# The payload must survive the deadline plumbing: it is handed to the policy
# through a file precisely because an async command's stdin would otherwise be
# reassigned to /dev/null, which reads as an empty payload and passes everything.
still_blocked="$(
  printf '%s' "$payload" \
    | HARN_BIN="$HARN_BIN" "$fixture_root/scripts/agent-shell-guard.sh"
)"
if [[ "$still_blocked" != *'"permissionDecision":"deny"'* ]]; then
  echo "adapter lost the payload through the deadline wrapper" >&2
  printf '%s\n' "$still_blocked" >&2
  exit 1
fi

# Resolution order. This hook runs before every shell call, so a cargo `debug`
# artifact must never win over a release-grade interpreter -- it starts slower
# and is the file cargo rewrites mid-build. Stand up a fixture repo whose
# harn_bin.sh advertises a debug build while a release build also exists.
mkdir -p "$order_root/scripts" "$order_root/target/release" "$order_root/dev-target/debug"
cp "$repo_root/scripts/agent-shell-guard.sh" "$order_root/scripts/"
cp "$repo_root/scripts/agent_shell_guard.harn" "$order_root/scripts/"
cp "$repo_root/scripts/agent_shell_guard_policy.harn" "$order_root/scripts/"

cat >"$order_root/dev-target/debug/harn" <<'STUB'
#!/usr/bin/env bash
echo "DEBUG-INTERPRETER-RAN"
STUB
cat >"$order_root/target/release/harn" <<'STUB'
#!/usr/bin/env bash
echo "RELEASE-INTERPRETER-RAN"
STUB
cat >"$order_root/scripts/harn_bin.sh" <<STUB
#!/usr/bin/env bash
printf '%s\n' "$order_root/dev-target/debug/harn"
STUB
chmod +x "$order_root/dev-target/debug/harn" \
  "$order_root/target/release/harn" "$order_root/scripts/harn_bin.sh"

# A hook-owned runtime can bypass project initialization only when its sidecar
# attests the exact standalone capability understood by this adapter.
mkdir -p "$order_root/hook-bin"
cat >"$order_root/hook-bin/harn" <<'STUB'
#!/usr/bin/env bash
printf 'HOOK-ARG=%s\n' "$@"
STUB
chmod +x "$order_root/hook-bin/harn"
printf '%s\n' 'harn-run-standalone-v1' >"$order_root/hook-bin/harn.standalone-v1"

explicit_resolved="$(
  printf '%s' '{}' \
    | HARN_BIN="$order_root/hook-bin/harn" \
      "$order_root/scripts/agent-shell-guard.sh"
)"
if [[ "$explicit_resolved" != *'HOOK-ARG=run'* ]] \
  || [[ "$explicit_resolved" == *'HOOK-ARG=--standalone'* ]]; then
  echo "explicit HARN_BIN did not preserve project-aware compatibility" >&2
  printf '%s\n' "$explicit_resolved" >&2
  exit 1
fi

standalone_resolved="$(
  printf '%s' '{}' \
    | env -u HARN_BIN \
      AGENT_SHELL_GUARD_HARN_BIN="$order_root/hook-bin/harn" \
      "$order_root/scripts/agent-shell-guard.sh"
)"
if [[ "$standalone_resolved" != *'HOOK-ARG=run'* ]] \
  || [[ "$standalone_resolved" != *'HOOK-ARG=--standalone'* ]] \
  || [[ "$standalone_resolved" != *'HOOK-ARG=--allow=command_risk_scan'* ]] \
  || [[ "$standalone_resolved" != *"HOOK-ARG=$order_root/scripts/agent_shell_guard.harn"* ]]; then
  echo "adapter did not select the attested standalone runtime" >&2
  printf '%s\n' "$standalone_resolved" >&2
  exit 1
fi

printf '%s\r\n' 'harn-run-standalone-v1' >"$order_root/hook-bin/harn.standalone-v1"
crlf_marker_resolved="$(
  printf '%s' '{}' \
    | env -u HARN_BIN \
      AGENT_SHELL_GUARD_HARN_BIN="$order_root/hook-bin/harn" \
      "$order_root/scripts/agent-shell-guard.sh"
)"
if [[ "$crlf_marker_resolved" != *'HOOK-ARG=--standalone'* ]]; then
  echo "CRLF standalone attestation was not selected" >&2
  printf '%s\n' "$crlf_marker_resolved" >&2
  exit 1
fi

printf '%s\n%s\n' 'harn-run-standalone-v1' 'unexpected-extra' \
  >"$order_root/hook-bin/harn.standalone-v1"
extra_marker_resolved="$(
  printf '%s' '{}' \
    | env -u HARN_BIN \
      AGENT_SHELL_GUARD_HARN_BIN="$order_root/hook-bin/harn" \
      "$order_root/scripts/agent-shell-guard.sh"
)"
if [[ "$extra_marker_resolved" != *RELEASE-INTERPRETER-RAN* ]] \
  || [[ "$extra_marker_resolved" == *HOOK-ARG=* ]]; then
  echo "non-exact standalone attestation displaced the project fallback" >&2
  printf '%s\n' "$extra_marker_resolved" >&2
  exit 1
fi

printf '%s\n' 'harn-run-standalone-v2' >"$order_root/hook-bin/harn.standalone-v1"
wrong_marker_resolved="$(
  printf '%s' '{}' \
    | env -u HARN_BIN \
      AGENT_SHELL_GUARD_HARN_BIN="$order_root/hook-bin/harn" \
      "$order_root/scripts/agent-shell-guard.sh"
)"
if [[ "$wrong_marker_resolved" != *RELEASE-INTERPRETER-RAN* ]] \
  || [[ "$wrong_marker_resolved" == *HOOK-ARG=* ]]; then
  echo "unknown standalone attestation displaced the project fallback" >&2
  printf '%s\n' "$wrong_marker_resolved" >&2
  exit 1
fi

rm "$order_root/hook-bin/harn.standalone-v1"
unattested_resolved="$(
  printf '%s' '{}' \
    | env -u HARN_BIN \
      AGENT_SHELL_GUARD_HARN_BIN="$order_root/hook-bin/harn" \
      "$order_root/scripts/agent-shell-guard.sh"
)"
if [[ "$unattested_resolved" != *RELEASE-INTERPRETER-RAN* ]] \
  || [[ "$unattested_resolved" == *HOOK-ARG=* ]]; then
  echo "unattested hook runtime displaced the project fallback" >&2
  printf '%s\n' "$unattested_resolved" >&2
  exit 1
fi

resolved="$(
  printf '%s' '{}' \
    | env -u HARN_BIN \
      AGENT_SHELL_GUARD_HARN_BIN="$order_root/missing-hook-harn" \
      "$order_root/scripts/agent-shell-guard.sh"
)"
if [[ "$resolved" != *RELEASE-INTERPRETER-RAN* ]]; then
  echo "adapter preferred a debug build over an available release interpreter" >&2
  printf '%s\n' "$resolved" >&2
  exit 1
fi

# ...but a debug build still beats no guard at all. With no release build and no
# harn on PATH, the advertised debug candidate must still be used.
rm -f "$order_root/target/release/harn"
fallback_resolved="$(
  printf '%s' '{}' \
    | env -u HARN_BIN PATH=/usr/bin:/bin \
      AGENT_SHELL_GUARD_HARN_BIN="$order_root/missing-hook-harn" \
      "$order_root/scripts/agent-shell-guard.sh"
)"
if [[ "$fallback_resolved" != *DEBUG-INTERPRETER-RAN* ]]; then
  echo "adapter dropped its debug-build fallback and left the shell unguarded" >&2
  printf '%s\n' "$fallback_resolved" >&2
  exit 1
fi

echo "agent_shell_guard_adapter_test: ok"
