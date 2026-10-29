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

# A fast policy and its cancelled watchdog race over the decision file. Every
# invocation here must forward a known nonempty denial, unlike a real allow
# whose intentionally empty output cannot expose premature file cleanup.
cat >"$fixture_root/fast-deny-harn" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"custody probe"}}'
STUB
chmod +x "$fixture_root/fast-deny-harn"
custody_runs=100
custody_failures=0
for ((custody_run = 0; custody_run < custody_runs; custody_run++)); do
  custody_output="$(printf '%s' "$payload" \
    | HARN_BIN="$fixture_root/fast-deny-harn" "$fixture_root/scripts/agent-shell-guard.sh")"
  if [[ "$custody_output" != *'"permissionDecisionReason":"custody probe"'* ]]; then
    custody_failures=$((custody_failures + 1))
  fi
done
printf 'decision custody: runs=%s missing_or_changed=%s\n' "$custody_runs" "$custody_failures"
[[ "$custody_failures" == 0 ]] || exit 1

allowed="$(
  printf '%s' '{"tool_name":"Bash","tool_input":{"command":"make check"}}' \
    | HARN_BIN="$HARN_BIN" "$fixture_root/scripts/agent-shell-guard.sh"
)"
if [[ -n "$allowed" ]]; then
  echo "adapter emitted a decision for the supported Make target" >&2
  printf '%s\n' "$allowed" >&2
  exit 1
fi

# Exercise the syntax parser under the real hook's restricted builtin grants.
# A denial must name the stash rule so a fault or deadline cannot pass as proof.
heredoc_literal="$(
  printf '%s' '{"tool_name":"Bash","tool_input":{"command":"printf '\''%s'\'' '\''$(git stash drop)'\''\ncat <<EOF\nhello\nEOF"}}' \
    | HARN_BIN="$HARN_BIN" "$fixture_root/scripts/agent-shell-guard.sh"
)"
if [[ -n "$heredoc_literal" ]]; then
  echo "adapter refused a literal substitution outside an expanding heredoc" >&2
  printf '%s\n' "$heredoc_literal" >&2
  exit 1
fi
for heredoc_payload in \
  '{"tool_name":"Bash","tool_input":{"command":"cat <<EOF\n'\''`git stash drop`'\''\nEOF"}}' \
  '{"tool_name":"Bash","tool_input":{"command":"cat <<EOF\n$( # )\ngit stash drop\n)\nEOF"}}' \
  '{"tool_name":"Bash","tool_input":{"command":"cat <<EOF | git stash drop\nhello\nEOF"}}' \
  '{"tool_name":"Bash","tool_input":{"command":"printf é; cat <<EOF\n$(git stash drop)\nEOF"}}'; do
  heredoc_denied="$(
    printf '%s' "$heredoc_payload" \
      | HARN_BIN="$HARN_BIN" "$fixture_root/scripts/agent-shell-guard.sh"
  )"
  if [[ "$heredoc_denied" != *'"permissionDecision":"deny"'* ]] \
    || [[ "$heredoc_denied" != *'git stash'* ]]; then
    echo "adapter did not refuse an executed heredoc or pipeline stash mutation" >&2
    printf '%s\n' "$heredoc_denied" >&2
    exit 1
  fi
done

# The verdict must not depend on the caller's directory. `git -C <repo>` with a
# backslash in the command resolves `worktree` through the alias lookup, which
# runs git inside the interpreter sandbox. That sandbox used to read only the
# caller's directory tree, so from a subdirectory the lookup exited 128 and the
# command was refused with the stash message.
cwd_repo="$repo_root"
cat >"$fixture_root/standalone-harn" <<'SH'
#!/usr/bin/env bash
exec "$STANDALONE_GUARD_RUNTIME" "$@"
SH
chmod +x "$fixture_root/standalone-harn"
printf '%s\n' 'harn-run-standalone-v1' >"$fixture_root/standalone-harn.standalone-v1"
cwd_guard() {
  env -u HARN_BIN STANDALONE_GUARD_RUNTIME="$HARN_BIN" \
    AGENT_SHELL_GUARD_HARN_BIN="$fixture_root/standalone-harn" \
    "$fixture_root/scripts/agent-shell-guard.sh"
}
cwd_read='{"tool_name":"Bash","tool_input":{"command":"gh pr view 1; git -C '"$cwd_repo"' worktree list | grep -i \"a\\|b\""}}'
cwd_stash='{"tool_name":"Bash","tool_input":{"command":"git stash push"}}'
for cwd_caller in "$cwd_repo" "$cwd_repo/scripts/tests"; do
  cwd_allowed="$(cd "$cwd_caller" && printf '%s' "$cwd_read" | cwd_guard)"
  if [[ -n "$cwd_allowed" ]]; then
    echo "adapter refused an ordinary read from $cwd_caller" >&2
    printf '%s\n' "$cwd_allowed" >&2
    exit 1
  fi
  # The other side: the real stash rule still fires from the same directory.
  cwd_denied="$(cd "$cwd_caller" && printf '%s' "$cwd_stash" | cwd_guard)"
  if [[ "$cwd_denied" != *'"permissionDecision":"deny"'* ]] \
    || [[ "$cwd_denied" != *'is refused: the stash list'* ]]; then
    echo "adapter did not refuse git stash from $cwd_caller" >&2
    printf '%s\n' "$cwd_denied" >&2
    exit 1
  fi
done
# A lookup git cannot complete is a guard fault, never a stash refusal.
cwd_fault="$(printf '%s' '{"tool_name":"Bash","tool_input":{"command":"git -C /nonexistent-guard-target s"}}' | cwd_guard)"
if [[ "$cwd_fault" != *'"permissionDecision":"deny"'* ]] \
  || [[ "$cwd_fault" != *'guard fault'* ]] \
  || [[ "$cwd_fault" == *'is refused: the stash list'* ]]; then
  echo "adapter reported an alias lookup fault as a stash refusal" >&2
  printf '%s\n' "$cwd_fault" >&2
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

# Raw worktree creation is plain Git, however it is spelled, and the guard stays
# live beside it: the stash refusal is the negative control for the allows.
for worktree_command in \
  "git worktree add -b lane ../lane origin/main" \
  "git -C $fixture_root worktree add ../lane origin/main" \
  "bash -lc 'git worktree add ../lane origin/main'" \
  "cd $fixture_root && git worktree add ../lane origin/main"; do
  worktree_allowed="$(
    printf '%s' "{\"tool_name\":\"Bash\",\"tool_input\":{\"command\":\"$worktree_command\"}}" \
      | HARN_BIN="$HARN_BIN" "$fixture_root/scripts/agent-shell-guard.sh"
  )"
  if [[ -n "$worktree_allowed" ]]; then
    echo "adapter refused plain worktree creation: $worktree_command" >&2
    printf '%s\n' "$worktree_allowed" >&2
    exit 1
  fi
done
worktree_control="$(
  printf '%s' '{"tool_name":"Bash","tool_input":{"command":"git stash"}}' \
    | HARN_BIN="$HARN_BIN" "$fixture_root/scripts/agent-shell-guard.sh"
)"
if [[ "$worktree_control" != *'"permissionDecision":"deny"'* ]]; then
  echo "adapter did not refuse git stash beside the allowed worktree add" >&2
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
# interpreter is the one case that still allows: settled before the policy runs,
# or reported by a runner wrapper as status 127.
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

# A wrapper that resolves no runtime exits 127. A fresh cloud clone hits this
# before its runtime is fetched, and denying there blocks the fetch itself.
missing_runtime_output="$(
  printf '%s' "$payload" \
    | GUARD_STATUS=127 HARN_BIN="$fixture_root/status-harn" \
      "$fixture_root/scripts/agent-shell-guard.sh" 2>"$fixture_root/missing-runtime.err"
)"
if [[ -n "$missing_runtime_output" ]] \
    || ! grep -Fq "agent shell guard is OFF" "$fixture_root/missing-runtime.err"; then
  echo "adapter did not fail open audibly when the runner found no runtime" >&2
  printf '%s\n' "$missing_runtime_output" >&2
  exit 1
fi

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
  || [[ "$standalone_resolved" != *'HOOK-ARG=--allow=bytes_from_string,command_risk_scan,hostlib_ast_search,lowercase,range'* ]] \
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

# Exercise the attested standalone interpreter: an explicit HARN_BIN selects
# the project host and cannot prove the standalone capability grants.
standalone_harn="$(cd "$(dirname "$HARN_BIN")" && pwd -P)/$(basename "$HARN_BIN")"
ln -s "$standalone_harn" "$make_root/standalone-harn"
printf '%s\n' 'harn-run-standalone-v1' >"$make_root/standalone-harn.standalone-v1"
standalone_verdict() {
  env -u HARN_BIN AGENT_SHELL_GUARD_HARN_BIN="$make_root/standalone-harn" \
    "$make_root/scripts/agent-shell-guard.sh"
}

windows_temp="$(
  printf '%s' '{"tool_name":"Bash","tool_input":{"command":"trash %TEMP%/build.log"}}' \
    | standalone_verdict
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
    | standalone_verdict
)"
if [[ -n "$user_file" ]]; then
  echo "the disposable-path rule refused a user file, which it must never do" >&2
  printf '%s\n' "$user_file" >&2
  exit 1
fi

git_read="$(
  printf '%s' '{"tool_name":"Bash","tool_input":{"command":"git log -1 --format=%H"}}' \
    | standalone_verdict
)"
if [[ -n "$git_read" ]]; then
  echo "standalone assignment analysis refused an ordinary Git format argument" >&2
  printf '%s\n' "$git_read" >&2
  exit 1
fi

# On POSIX, backslashes name a relative directory, even when they resemble a
# Windows UNC path. A hook must not execute that directory's Git during preflight.
mkdir -p "$make_root/\\\\untrusted"
cat >"$make_root/\\\\untrusted/git" <<EOF
#!/bin/sh
printf reached > "$make_root/untrusted-git-ran"
EOF
chmod +x "$make_root/\\\\untrusted/git"
untrusted_verdict="$(
  cd "$make_root"
  PATH='\\untrusted:/usr/bin:/bin' git --list-cmds=builtins
  [[ -e "$make_root/untrusted-git-ran" ]] || { echo "fake Git control did not run" >&2; exit 1; }
  rm "$make_root/untrusted-git-ran"
  printf '%s' '{"tool_name":"Bash","tool_input":{"command":"git history"}}' \
    | OS=Windows_NT PATH='\\untrusted:/usr/bin:/bin' standalone_verdict
)"
if [[ -e "$make_root/untrusted-git-ran" ]]; then
  echo "preflight executed a POSIX relative directory disguised as a Windows path" >&2
  exit 1
fi
if [[ "$untrusted_verdict" != *'"permissionDecision":"deny"'* ]] \
  || [[ "$untrusted_verdict" != *'git stash'* ]] \
  || [[ "$untrusted_verdict" == *'policy faulted'* ]]; then
  echo "the guard did not refuse an unmeasured Git alias" >&2
  printf '%s\n' "$untrusted_verdict" >&2
  exit 1
fi

commented_substitution="$(
  standalone_verdict <<'PAYLOAD'
{"tool_name":"Bash","tool_input":{"command":"echo \"$( # )\ngit stash drop\n)\""}}
PAYLOAD
)"
if [[ "$commented_substitution" != *'"permissionDecision":"deny"'* ]] \
  || [[ "$commented_substitution" != *'git stash'* ]] \
  || [[ "$commented_substitution" == *'policy faulted'* ]]; then
  echo "the guard missed a stash write after a shell comment in a substitution" >&2
  printf '%s\n' "$commented_substitution" >&2
  exit 1
fi
commented_read="$(
  standalone_verdict <<'PAYLOAD'
{"tool_name":"Bash","tool_input":{"command":"echo \"$( # )\ngit stash list\n)\""}}
PAYLOAD
)"
if [[ -n "$commented_read" ]]; then
  echo "the guard rejected a read-only stash substitution after a shell comment" >&2
  printf '%s\n' "$commented_read" >&2
  exit 1
fi

heredoc_write="$(
  standalone_verdict <<'PAYLOAD'
{"tool_name":"Bash","tool_input":{"command":"cat <<'EOF'\n'\nEOF\necho \"$(git stash pop)\""}}
PAYLOAD
)"
if [[ "$heredoc_write" != *'"permissionDecision":"deny"'* ]] \
  || [[ "$heredoc_write" != *'git stash'* ]] \
  || [[ "$heredoc_write" == *'policy faulted'* ]]; then
  echo "the guard missed an executed stash write after a literal heredoc" >&2
  printf '%s\n' "$heredoc_write" >&2
  exit 1
fi
heredoc_dash="$(
  standalone_verdict <<'PAYLOAD'
{"tool_name":"Bash","tool_input":{"command":"cat <<'EOF'\n'\nEOF\ndash -c 'git stash pop'"}}
PAYLOAD
)"
if [[ "$heredoc_dash" != *'"permissionDecision":"deny"'* ]] \
  || [[ "$heredoc_dash" != *'git stash'* ]] \
  || [[ "$heredoc_dash" == *'policy faulted'* ]]; then
  echo "the guard missed a nested dash command after a literal heredoc" >&2
  printf '%s\n' "$heredoc_dash" >&2
  exit 1
fi
heredoc_literal="$(
  standalone_verdict <<'PAYLOAD'
{"tool_name":"Bash","tool_input":{"command":"cat <<'EOF'\ngit stash pop\nEOF"}}
PAYLOAD
)"
if [[ -n "$heredoc_literal" ]]; then
  echo "the guard treated literal heredoc text as an executed command" >&2
  printf '%s\n' "$heredoc_literal" >&2
  exit 1
fi

for stash_command in 'WORKTREE_MODE=review git stash push' 'git -c alias.hidden=stash hidden'; do
  stash_verdict="$(
    printf '{"tool_name":"Bash","tool_input":{"command":"%s"}}' "$stash_command" \
      | standalone_verdict
  )"
  if [[ "$stash_verdict" != *'"permissionDecision":"deny"'* ]] \
    || [[ "$stash_verdict" != *'git stash'* ]] \
    || [[ "$stash_verdict" == *'policy faulted'* ]]; then
    echo "standalone assignment analysis did not reach the stash rule: $stash_command" >&2
    printf '%s\n' "$stash_verdict" >&2
    exit 1
  fi
done

config_alias_verdict="$(
  printf '%s' '{"tool_name":"Bash","tool_input":{"command":"git config alias.hidden stash; git hidden push"}}' \
    | standalone_verdict
)"
if [[ "$config_alias_verdict" != *'"permissionDecision":"deny"'* ]] \
  || [[ "$config_alias_verdict" != *'git stash'* ]] \
  || [[ "$config_alias_verdict" == *'policy faulted'* ]]; then
  echo "standalone guard trusted alias configuration changed by the payload" >&2
  exit 1
fi

system_builtins="$(/usr/bin/git --list-cmds=builtins)"
if [[ $'\n'"$system_builtins"$'\n' != *$'\nhistory\n'* ]]; then
  history_verdict="$(
    printf '%s' '{"tool_name":"Bash","tool_input":{"command":"/usr/bin/git history"}}' \
      | GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=alias.history GIT_CONFIG_VALUE_0='stash push' standalone_verdict
  )"
  if [[ "$history_verdict" != *'"permissionDecision":"deny"'* ]] \
    || [[ "$history_verdict" != *'git stash'* ]] \
    || [[ "$history_verdict" == *'policy faulted'* ]]; then
    echo "standalone guard assumed a builtin absent from the selected Git" >&2
    exit 1
  fi
  echo "selected-Git history alias control: reached"
else
  echo "selected-Git history alias control: unmeasured, this Git owns the builtin"
fi

echo "agent_shell_guard_adapter_test: ok"
