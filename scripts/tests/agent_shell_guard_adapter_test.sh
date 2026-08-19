#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
if [[ -z "${HARN_BIN:-}" || ! -x "$HARN_BIN" ]]; then
  echo "agent shell guard adapter test requires an executable HARN_BIN" >&2
  exit 1
fi

fixture_root="$(mktemp -d)"
trap 'rm -rf "$fixture_root"' EXIT
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
if [[ "$blocked" != *'"permissionDecision":"deny"'* ]] \
  || [[ "$blocked" != *'Run `make check` instead.'* ]]; then
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
# deadline and fails open well inside that budget.
cat >"$fixture_root/hanging-harn" <<'STUB'
#!/usr/bin/env bash
sleep 120
STUB
chmod +x "$fixture_root/hanging-harn"

deadline_start="$(date +%s)"
hung_output="$(
  printf '%s' "$payload" \
    | HARN_BIN="$fixture_root/hanging-harn" \
      AGENT_SHELL_GUARD_DEADLINE_SECONDS=2 \
      "$fixture_root/scripts/agent-shell-guard.sh"
)"
deadline_elapsed="$(( $(date +%s) - deadline_start ))"
if [[ -n "$hung_output" ]]; then
  echo "adapter emitted a decision for a policy that never answered" >&2
  printf '%s\n' "$hung_output" >&2
  exit 1
fi
if (( deadline_elapsed > 15 )); then
  echo "adapter waited ${deadline_elapsed}s on a hanging policy instead of failing open at its 2s deadline" >&2
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

echo "agent_shell_guard_adapter_test: ok"
