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
  | "$HARN_BIN" run "$fixture_root/scripts/agent_shell_guard.harn" \
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

echo "agent_shell_guard_adapter_test: ok"
