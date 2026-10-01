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

discarded_blocked="$(
  printf '%s' \
    '{"tool_name":"Bash","tool_input":{"command":"make test | tail -20"}}' \
    | HARN_BIN="$HARN_BIN" "$fixture_root/scripts/agent-shell-guard.sh"
)"
if [[ "$discarded_blocked" != *'"permissionDecision":"deny"'* ]] \
  || [[ "$discarded_blocked" != *'output is piped into a filter'* ]]; then
  echo "adapter did not preserve the piped-build output denial" >&2
  printf '%s\n' "$discarded_blocked" >&2
  exit 1
fi

# The worktree rule answers differently depending on whether the repository the
# guard is vendored in owns an admission command. The wrapper measures its own
# tree, so both directions need their own fixture root rather than a different
# payload, and both are exercised end to end through the adapter.
# The rule answers for the repository the command targets, so each direction
# points `-C` at a root this test built and knows the answer for. A repository
# marker is what the wrapper walks up to find, and both roots need one before
# they can be measured at all.
mkdir -p "$fixture_root/.git"

admitted_root="$(cd "$(mktemp -d)" && pwd -P)"
trap 'rm -rf "$admitted_root"' EXIT
mkdir -p "$admitted_root/scripts" "$admitted_root/.git"
cp "$repo_root/scripts/agent-shell-guard.sh" "$admitted_root/scripts/"
cp "$repo_root/scripts/agent_shell_guard.harn" "$admitted_root/scripts/"
cp "$repo_root/scripts/agent_shell_guard_policy.harn" "$admitted_root/scripts/"
printf '// admission\n' >"$admitted_root/scripts/fleet-worktree-admit.ts"
worktree_blocked="$(
  printf '%s' "{\"tool_name\":\"Bash\",\"tool_input\":{\"command\":\"bash -lc 'git -C $admitted_root worktree add ../unowned origin/main'\"}}" \
    | HARN_BIN="$HARN_BIN" "$admitted_root/scripts/agent-shell-guard.sh"
)"
if [[ "$worktree_blocked" != *'"permissionDecision":"deny"'* ]] \
  || [[ "$worktree_blocked" != *'fleet-worktree-admit'* ]]; then
  echo "adapter did not route nested raw worktree creation to Fleet admission" >&2
  printf '%s\n' "$worktree_blocked" >&2
  exit 1
fi

# The same command where the guard's own repository owns no admission command
# must be allowed. Naming a command the operator cannot run is the failure this
# direction guards, and an empty verdict is how the adapter says "allowed".
worktree_allowed="$(
  printf '%s' "{\"tool_name\":\"Bash\",\"tool_input\":{\"command\":\"bash -lc 'git -C $fixture_root worktree add ../unowned origin/main'\"}}" \
    | HARN_BIN="$HARN_BIN" "$fixture_root/scripts/agent-shell-guard.sh"
)"
if [[ -n "$worktree_allowed" ]]; then
  echo "adapter refused raw worktree creation where the repository owns no admission command" >&2
  printf '%s\n' "$worktree_allowed" >&2
  exit 1
fi

# A target outside every measured root is not a target found to own nothing.
# A path *inside* a measured repository is covered by that repository's row,
# which is why this one points outside them all.
# Without this arm the fix is a hole: the easy version of "answer for the
# repository the command targets" allows anything it failed to resolve, which
# is the whole absence-reads-as-success shape this rule exists to avoid.
worktree_unmeasured="$(
  printf '%s' "{\"tool_name\":\"Bash\",\"tool_input\":{\"command\":\"git -C /no-such-repository-8447/lane worktree add ../unowned origin/main\"}}" \
    | HARN_BIN="$HARN_BIN" "$admitted_root/scripts/agent-shell-guard.sh"
)"
if [[ "$worktree_unmeasured" != *'"permissionDecision":"deny"'* ]] \
  || [[ "$worktree_unmeasured" != *'could not tell'* ]]; then
  echo "adapter treated an unmeasured target repository as one owning no admission command" >&2
  printf '%s\n' "$worktree_unmeasured" >&2
  exit 1
fi

wrapped_worktree_blocked="$(
  printf '%s' \
    '{"tool_name":"Bash","tool_input":{"command":"/usr/bin/env -u GIT_DIR command git --no-pager worktree add ../unowned origin/main"}}' \
    | HARN_BIN="$HARN_BIN" "$admitted_root/scripts/agent-shell-guard.sh"
)"
if [[ "$wrapped_worktree_blocked" != *'"permissionDecision":"deny"'* ]] \
  || [[ "$wrapped_worktree_blocked" != *'fleet-worktree-admit'* ]]; then
  echo "adapter allowed worktree creation through process launchers" >&2
  printf '%s\n' "$wrapped_worktree_blocked" >&2
  exit 1
fi

admission_allowed="$(
  printf '%s' \
    '{"tool_name":"Bash","tool_input":{"command":"./scripts/fleet-worktree-admit --issue-url https://github.com/acme/repo/issues/1"}}' \
    | HARN_BIN="$HARN_BIN" "$fixture_root/scripts/agent-shell-guard.sh"
)"
if [[ -n "$admission_allowed" ]]; then
  echo "adapter denied the owning Fleet admission command" >&2
  printf '%s\n' "$admission_allowed" >&2
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
#
# The stub ignores TERM until the descendant's pid is on disk. Bash runs a TERM
# trap between commands, so with the trap armed first a deadline that expired
# right after the fork exited the stub before the write, and the survival check
# below had no pid to test. A stub that has not reached its first line when the
# deadline expires can still be stopped before the write; the check below
# reports that as a fixture race.
cat >"$fixture_root/hanging-harn" <<'STUB'
#!/usr/bin/env bash
trap '' TERM
(trap '' TERM; while :; do sleep 1; done) &
printf '%s\n' "$!" >"$GUARD_CHILD_PID_FILE"
trap 'exit 0' TERM
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
# An absent or empty pid file means the fixture never recorded its descendant.
# That is a fixture race, not evidence about the adapter, so it gets its own
# message instead of reading as a survivor or aborting inside `cat`.
if [[ ! -e "$fixture_root/hanging-child.pid" ]]; then
  echo "fixture race: the hanging policy stub was stopped before it recorded its descendant's pid, so descendant survival was not checked" >&2
  exit 1
fi
hanging_child_pid="$(cat "$fixture_root/hanging-child.pid")"
if [[ -z "$hanging_child_pid" ]]; then
  echo "fixture race: the hanging policy stub left an empty pid file, so descendant survival was not checked" >&2
  exit 1
fi
if kill -0 "$hanging_child_pid" 2>/dev/null; then
  echo "adapter left a TERM-ignoring policy descendant running: $hanging_child_pid" >&2
  exit 1
fi

# Timeout and signal-shaped exits mean the policy produced no trustworthy
# decision, so all three statuses deny. Every other non-zero status after the
# policy has started denies as well: the interpreter was present and runnable,
# the evaluation failed, and no rule was applied. An absent or non-executable
# interpreter is the one case that still allows, settled before the policy runs.
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
if [[ "$crash_output" != *'"permissionDecision":"deny"'* ]]; then
  echo "adapter did not deny after the policy crashed" >&2
  printf '%s\n' "$crash_output" >&2
  exit 1
fi
if [[ "$crash_output" == *"must-not-escape"* ]]; then
  echo "adapter let a partial verdict escape a crashed policy" >&2
  printf '%s\n' "$crash_output" >&2
  exit 1
fi

# A policy that throws is the shape that made this fail-open costly: the
# in-process suite stays green because every rule still answers, while the
# host reads the adapter's silence as an allow and runs the command. Keep the
# throwing policy as a permanent fixture so that combination cannot return.
mkdir -p "$fixture_root/throwing"
cp "$repo_root/scripts/agent-shell-guard.sh" "$fixture_root/throwing/"
cat >"$fixture_root/throwing/agent_shell_guard.harn" <<'HARN'
fn main(harness: Harness) {
  throw "deliberate top-of-decision fault"
}
HARN
throw_output="$(
  printf '%s' "$payload" \
    | HARN_BIN="$HARN_BIN" "$fixture_root/throwing/agent-shell-guard.sh"
)"
if [[ "$throw_output" != *'"permissionDecision":"deny"'* ]]; then
  echo "adapter did not deny a policy that threw before deciding" >&2
  printf '%s\n' "$throw_output" >&2
  exit 1
fi
if [[ "$throw_output" != *"deliberate top-of-decision fault"* ]]; then
  echo "adapter denied without naming the thrown reason" >&2
  printf '%s\n' "$throw_output" >&2
  exit 1
fi

# The one surviving fail-open. It stays an allow so the setup that installs the
# interpreter is still runnable, but it must be audible: an allow that says
# nothing is the same silence the fault path above was fixed to stop emitting.
unavailable_output="$(
  printf '%s' "$payload" \
    | env -u HARN_BIN PATH=/usr/bin:/bin \
      AGENT_SHELL_GUARD_HARN_BIN="$fixture_root/missing-harn" \
      "$fixture_root/scripts/agent-shell-guard.sh" 2>"$fixture_root/unavailable.err"
)"
if [[ -n "$unavailable_output" ]]; then
  echo "adapter did not fail open when no interpreter was available" >&2
  printf '%s\n' "$unavailable_output" >&2
  exit 1
fi
if ! grep -Fq "agent shell guard is OFF" "$fixture_root/unavailable.err"; then
  echo "adapter allowed silently when no interpreter was available" >&2
  cat "$fixture_root/unavailable.err" >&2
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

printf '%s\r' 'harn-run-standalone-v1' >"$order_root/hook-bin/harn.standalone-v1"
bare_cr_marker_resolved="$(
  printf '%s' '{}' \
    | env -u HARN_BIN \
      AGENT_SHELL_GUARD_HARN_BIN="$order_root/hook-bin/harn" \
      "$order_root/scripts/agent-shell-guard.sh"
)"
if [[ "$bare_cr_marker_resolved" != *RELEASE-INTERPRETER-RAN* ]] \
  || [[ "$bare_cr_marker_resolved" == *HOOK-ARG=* ]]; then
  echo "bare-CR standalone attestation displaced the project fallback" >&2
  printf '%s\n' "$bare_cr_marker_resolved" >&2
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

# The Make-target measurement, end to end through the adapter. The policy
# cannot read the filesystem, so this is the only place that proves the
# wrapper's census reaches it. Both arms run against the same fixture with
# only the Makefile changed, which is the one variable under test.
make_root="$(cd "$(mktemp -d)" && pwd -P)"
trap 'rm -rf "$fixture_root" "$order_root" "$make_root"' EXIT
mkdir -p "$make_root/scripts"
cp "$repo_root/scripts/agent-shell-guard.sh" "$make_root/scripts/"
cp "$repo_root/scripts/agent_shell_guard.harn" "$make_root/scripts/"
cp "$repo_root/scripts/agent_shell_guard_policy.harn" "$make_root/scripts/"

swift_payload='{"tool_name":"Bash","tool_input":{"command":"swift test"}}'

# A repository whose Makefile owns the target. The assignment and the
# dot-directive are there so the reader cannot mistake a loose match for a
# real declaration.
cat >"$make_root/Makefile" <<'MAKE'
.PHONY: build swift-build swift-test
SWIFT_FLAGS := --disable-sandbox
build:
	echo build
swift-build:
	echo swift build
swift-test:
	echo swift test
MAKE
owned="$(
  printf '%s' "$swift_payload" \
    | HARN_BIN="$HARN_BIN" "$make_root/scripts/agent-shell-guard.sh"
)"
if [[ "$owned" != *'"permissionDecision":"deny"'* ]] \
  || [[ "$owned" != *'Run `make swift-test` instead'* ]]; then
  echo "adapter did not refuse a bare swift test in a repository that owns the target" >&2
  printf '%s\n' "$owned" >&2
  exit 1
fi

# CONTROL: the same command in a repository whose Makefile does not declare it.
# Without this arm, a rule that denied unconditionally would pass the test
# above and then name a target the operator does not have.
cat >"$make_root/Makefile" <<'MAKE'
.PHONY: build
build:
	echo build
MAKE
unowned="$(
  printf '%s' "$swift_payload" \
    | HARN_BIN="$HARN_BIN" "$make_root/scripts/agent-shell-guard.sh"
)"
if [[ -n "$unowned" ]]; then
  echo "adapter refused a bare swift test in a repository with no swift-test target" >&2
  printf '%s\n' "$unowned" >&2
  exit 1
fi

# CONTROL: no Makefile at all is the same answer, and the Cargo rule, which is
# unconditional, still fires there. A silent census failure would otherwise
# look identical to this allow.
rm -f "$make_root/Makefile"
no_makefile="$(
  printf '%s' "$swift_payload" \
    | HARN_BIN="$HARN_BIN" "$make_root/scripts/agent-shell-guard.sh"
)"
if [[ -n "$no_makefile" ]]; then
  echo "adapter refused a bare swift test in a repository with no Makefile" >&2
  printf '%s\n' "$no_makefile" >&2
  exit 1
fi
still_guarded="$(
  printf '%s' '{"tool_name":"Bash","tool_input":{"command":"cargo check"}}' \
    | HARN_BIN="$HARN_BIN" "$make_root/scripts/agent-shell-guard.sh"
)"
if [[ "$still_guarded" != *'"permissionDecision":"deny"'* ]]; then
  echo "control: the guard produced no verdict at all, so the allows above prove nothing" >&2
  printf '%s\n' "$still_guarded" >&2
  exit 1
fi

# Every rule the policy owns must reach a verdict in the standalone host, not
# only in the full host the unit tests run in. An ungranted builtin does not
# degrade: it throws, the adapter fails closed, and the operator gets an
# interpreter error where a rule should be. The disposable-path matcher folds
# case for Windows spellings, and that fold is only reached past the POSIX temp
# roots, so the arm below is the one a unit test cannot stand in for.
windows_temp="$(
  printf '%s' '{"tool_name":"Bash","tool_input":{"command":"trash %TEMP%/build.log"}}' \
    | HARN_BIN="$HARN_BIN" "$make_root/scripts/agent-shell-guard.sh"
)"
if [[ "$windows_temp" != *'visible Trash'* ]]; then
  echo "the disposable-path rule did not reach a verdict in the standalone host" >&2
  printf '%s\n' "$windows_temp" >&2
  exit 1
fi

# CONTROL: the same rule on a user file allows. A fault would deny both, so
# without this arm the deny above could be an error message rather than a rule.
user_file="$(
  printf '%s' '{"tool_name":"Bash","tool_input":{"command":"trash /Users/alice/Documents/report.txt"}}' \
    | HARN_BIN="$HARN_BIN" "$make_root/scripts/agent-shell-guard.sh"
)"
if [[ -n "$user_file" ]]; then
  echo "the disposable-path rule refused a user file, which it must never do" >&2
  printf '%s\n' "$user_file" >&2
  exit 1
fi

echo "agent_shell_guard_adapter_test: ok"
